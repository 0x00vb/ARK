//! Agents talk among themselves and a chat reads it back as lines and a talk (P2, ADR 0119).
//! Scripted models only.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_interact::exchanges::{lines, talk};
use ark_orchestrate::*;
use ark_sdk::prelude::ark_model::{self, Completion, Message, Provider, ToolCall, ToolDef, Usage};
use ark_sdk::Ark;
use serde_json::{json, Value};

/// Answers each agent from its own list, so agents that run at once get their own replies.
struct ByAgent(Mutex<HashMap<String, VecDeque<Completion>>>, Vec<String>);

impl ByAgent {
    fn new(script: Vec<(&str, Vec<Completion>)>) -> Arc<Self> {
        Self::failing(script, &[])
    }

    /// `broken` agents' models answer with an error.
    fn failing(script: Vec<(&str, Vec<Completion>)>, broken: &[&str]) -> Arc<Self> {
        Arc::new(ByAgent(
            Mutex::new(
                script
                    .into_iter()
                    .map(|(a, r)| (a.to_string(), r.into()))
                    .collect(),
            ),
            broken.iter().map(|b| b.to_string()).collect(),
        ))
    }
}

#[async_trait::async_trait]
impl Provider for ByAgent {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        _tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let text = format!(
            "{system}\n{}",
            messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        if self
            .1
            .iter()
            .any(|b| text.contains(&format!("You are the agent \"{b}\"")))
        {
            return Err(ark_model::ProviderError::Api {
                provider: "m".into(),
                detail: "down".into(),
            });
        }
        let mut all = self.0.lock().unwrap();
        let found = all
            .iter_mut()
            .find(|(a, _)| text.contains(&format!("You are the agent \"{a}\"")));
        Ok(found
            .and_then(|(_, r)| r.pop_front())
            .unwrap_or_else(|| answer("done")))
    }

    fn describe(&self) -> String {
        "m".into()
    }
}

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

fn call(name: &str, args: Value) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c0".into(),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::measured(10, 5),
    }
}

fn said(text: &str) -> TaskSpec {
    let mut t = TaskSpec::new(text);
    t.person = true;
    t
}

fn start(to: &str, objective: &str) -> Completion {
    call("agents.start", json!({ "to": to, "objective": objective }))
}

fn note(to: &str, text: &str) -> Completion {
    call("agents.message", json!({ "to": to, "text": text }))
}

async fn settled(o: &Orchestrator, agent: &str) {
    for _ in 0..500 {
        let tasks: Vec<TaskRecord> = o.tasks().into_iter().filter(|t| t.owner == agent).collect();
        if !tasks.is_empty() && tasks.iter().all(|t| t.state.is_final()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{agent} did not finish");
}

struct Team {
    _dir: tempfile::TempDir,
    ark: Ark,
    o: Orchestrator,
    project: String,
}

async fn team(script: Vec<(&str, Vec<Completion>)>) -> Team {
    team_with(ByAgent::new(script)).await
}

async fn team_with(m: Arc<ByAgent>) -> Team {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("m", m as Arc<dyn Provider>))
        .build()
        .unwrap();
    let mut ceo = AgentSpec::new("ceo")
        .model("m")
        .delegates_to(["cmo", "cso"]);
    ceo.look = "owl".into();
    o.spawn(ceo).await.unwrap();
    let mut cmo = AgentSpec::new("cmo")
        .model("m")
        .delegates_to(["cso", "ceo"]);
    cmo.title = "CMO".into();
    o.spawn(cmo).await.unwrap();
    let mut cso = AgentSpec::new("cso").model("m");
    cso.title = "CSO".into();
    o.spawn(cso).await.unwrap();
    Team {
        _dir: dir,
        ark,
        o,
        project: project.id.as_str().to_string(),
    }
}

#[tokio::test]
async fn a_chats_turns_read_back_as_lines_and_a_talk() {
    let t = team(vec![
        (
            "ceo",
            vec![
                note("cmo", "plan the launch"),
                start("cmo", "write the copy"),
                answer("asked"),
                start("cso", "research buyers"),
                start("cmo", "review the copy"),
                answer("asked again"),
                note("cso", "a second chat"),
                answer("noted"),
            ],
        ),
        ("cmo", vec![answer("copy ready"), answer("reviewed")]),
        ("cso", vec![answer("buyers found")]),
    ])
    .await;
    let first = t.o.run("ceo", said("brief the board")).await.unwrap();
    let second = t.o.run("ceo", said("go on")).await.unwrap();
    settled(&t.o, "cmo").await;
    settled(&t.o, "cso").await;
    let chat_b = t.o.start_thread("ceo").unwrap();
    let mut other = said("another chat");
    other.thread = Some(chat_b.clone());
    let third = t.o.run("ceo", other).await.unwrap();
    t.o.run("ceo", said("hello")).await.unwrap();
    let engine = t.ark.engine();

    let default = lines(engine, &t.project, "ceo", None).unwrap();
    assert_eq!(default.len(), 2, "{default:?}");
    assert_eq!(default[0].turn, first.id);
    assert_eq!(
        default[0].label, "3 messages with CMO",
        "note, brief, reply"
    );
    assert_eq!(default[0].with.len(), 1);
    assert_eq!(default[1].turn, second.id);
    assert_eq!(default[1].label, "Messaged CSO and CMO");
    assert_eq!(default[1].count, 4);
    assert!(default.iter().all(|l| !l.failed));
    assert_eq!(default[0].with[0].name, "CMO");

    let b = lines(engine, &t.project, "ceo", Some(&chat_b)).unwrap();
    assert_eq!(b.len(), 1, "{b:?}");
    assert_eq!(b[0].turn, third.id);
    assert_eq!(b[0].label, "Messaged CSO");
    assert_eq!(
        lines(engine, &t.project, "ceo", Some("not-a-chat"))
            .unwrap()
            .len(),
        0
    );

    let cmo = lines(engine, &t.project, "cmo", None).unwrap();
    assert!(
        cmo.is_empty(),
        "a colleague's reply is not what its turns started: {cmo:?}"
    );

    let pair = talk(engine, &t.project, "ceo", "cmo").unwrap();
    assert_eq!(pair.with[0].agent, "ceo");
    assert_eq!(pair.with[1].name, "CMO");
    let said: Vec<(&str, &str)> = pair
        .messages
        .iter()
        .map(|m| (m.author.as_str(), m.text.as_str()))
        .collect();
    assert_eq!(
        said,
        vec![
            ("ceo", "plan the launch"),
            ("ceo", "write the copy"),
            ("cmo", "copy ready"),
            ("ceo", "review the copy"),
            ("cmo", "reviewed"),
        ]
    );
    assert!(pair.messages[0].work.is_none(), "a note carries no work");
    assert_eq!(
        pair.messages[1].work, pair.messages[2].work,
        "a brief and its reply share a task"
    );
    assert!(pair.messages.windows(2).all(|w| w[0].at_ms <= w[1].at_ms));
    let back = talk(engine, &t.project, "cmo", "ceo").unwrap();
    assert_eq!(back.with[0].agent, "cmo");
    assert_eq!(back.messages, pair.messages, "the same messages either way");
    assert!(talk(engine, &t.project, "cmo", "cso")
        .unwrap()
        .messages
        .is_empty());
    assert!(talk(engine, &t.project, "ceo", "nobody").is_err());
}

#[tokio::test]
async fn work_a_colleague_started_is_the_colleagues_line_and_inbound_notes_are_in_the_talk() {
    let t = team(vec![
        ("ceo", vec![start("cmo", "get buyers"), answer("asked")]),
        (
            "cmo",
            vec![
                start("cso", "research buyers"),
                note("ceo", "on it"),
                answer("working"),
            ],
        ),
        ("cso", vec![answer("buyers found")]),
    ])
    .await;
    let ask = t.o.run("ceo", said("get buyers")).await.unwrap();
    settled(&t.o, "cmo").await;
    settled(&t.o, "cso").await;
    let engine = t.ark.engine();
    let ceo = lines(engine, &t.project, "ceo", None).unwrap();
    assert_eq!(ceo.len(), 1, "{ceo:?}");
    assert_eq!(ceo[0].turn, ask.id);
    assert_eq!(
        ceo[0]
            .with
            .iter()
            .map(|w| w.agent.as_str())
            .collect::<Vec<_>>(),
        ["cmo"]
    );
    let cmo = lines(engine, &t.project, "cmo", None).unwrap();
    assert_eq!(cmo.len(), 1, "{cmo:?}");
    assert_eq!(cmo[0].label, "Messaged CSO and ceo", "{cmo:?}");
    let pair = talk(engine, &t.project, "ceo", "cmo").unwrap();
    assert!(
        pair.messages
            .iter()
            .any(|m| m.author == "cmo" && m.text == "on it"),
        "a note the colleague sent shows in the talk: {pair:?}"
    );
}

#[tokio::test]
async fn a_reply_that_failed_says_so_and_a_running_brief_has_no_reply_yet() {
    let t = team_with(ByAgent::failing(
        vec![(
            "ceo",
            vec![
                start("cmo", "write"),
                start("cso", "research"),
                answer("asked"),
            ],
        )],
        &["cmo"],
    ))
    .await;
    t.o.run("ceo", said("go")).await.unwrap();
    settled(&t.o, "cmo").await;
    settled(&t.o, "cso").await;
    let engine = t.ark.engine();
    let l = lines(engine, &t.project, "ceo", None).unwrap();
    assert_eq!(l.len(), 1);
    assert!(l[0].failed, "{l:?}");
    assert_eq!(
        l[0].label,
        "Messaged CMO and CSO. Reply from CMO did not arrive"
    );
    assert_eq!(
        l[0].with.len(),
        2,
        "the ask that worked is still in the line"
    );
    assert!(l[0].with[0].failed && !l[0].with[1].failed);
    let pair = talk(engine, &t.project, "ceo", "cmo").unwrap();
    assert_eq!(pair.messages.len(), 2);
    assert!(
        pair.messages[1].failed
            && pair.messages[1].author == "cmo"
            && pair.messages[1].text.is_empty()
    );

    // The same brief while its task still runs: no reply slot.
    let store = &engine.store;
    let mut task = store
        .list_records("orch_task", Some(&t.project))
        .unwrap()
        .into_iter()
        .find(|v| v["owner"] == "cmo")
        .unwrap();
    task["state"] = json!("running");
    task["error"] = Value::Null;
    task["result"] = Value::Null;
    task["finished_ms"] = Value::Null;
    let id = task["id"].as_str().unwrap().to_string();
    store
        .put_record("orch_task", &id, &t.project, &task)
        .unwrap();
    let pair = talk(engine, &t.project, "ceo", "cmo").unwrap();
    assert_eq!(pair.messages.len(), 1, "{pair:?}");
    assert!(pair.messages[0].pending, "its reply may still come");
    let l = lines(engine, &t.project, "ceo", None).unwrap();
    assert!(!l[0].failed);
    assert_eq!(l[0].label, "Messaged CMO and CSO", "{l:?}");

    // A stopped brief has no slot either.
    task["state"] = json!("cancelled");
    task["finished_ms"] = json!(5);
    store
        .put_record("orch_task", &id, &t.project, &task)
        .unwrap();
    let stopped = talk(engine, &t.project, "ceo", "cmo").unwrap();
    assert_eq!(stopped.messages.len(), 1);
    assert!(
        !stopped.messages[0].pending,
        "a stopped brief waits for nothing"
    );
}

#[tokio::test]
async fn the_persons_messages_older_notes_and_work_with_no_agent_are_not_exchanges() {
    let t = team(vec![("ceo", vec![answer("hi")])]).await;
    t.o.message("user", "cmo", "a note from the person")
        .unwrap();
    t.o.run("cmo", TaskSpec::new("the app asked"))
        .await
        .unwrap();
    t.o.run("ceo", said("hello")).await.unwrap();
    // A note saved before its words were kept.
    let store = &t.ark.engine().store;
    let id = "0000000000001-old";
    store
        .put_record(
            "orch_message",
            id,
            &t.project,
            &json!({ "from": "ceo", "to": "cmo", "task": "t-gone", "at_ms": 1 }),
        )
        .unwrap();
    let engine = t.ark.engine();
    for agent in ["ceo", "cmo", "cso"] {
        assert!(
            lines(engine, &t.project, agent, None).unwrap().is_empty(),
            "{agent}"
        );
    }
    assert!(talk(engine, &t.project, "ceo", "cmo")
        .unwrap()
        .messages
        .is_empty());
}

#[tokio::test]
async fn a_long_brief_is_clipped_in_the_read_and_three_thousand_tasks_read_in_two_seconds() {
    let long = "x".repeat(5000);
    let t = team(vec![("ceo", vec![start("cmo", &long), answer("asked")])]).await;
    t.o.run("ceo", said("go")).await.unwrap();
    settled(&t.o, "cmo").await;
    let engine = t.ark.engine();
    let pair = talk(engine, &t.project, "ceo", "cmo").unwrap();
    assert_eq!(
        pair.messages[0].text.chars().count(),
        ark_interact::exchanges::MAX_CHARS
    );
    // 3000 more task records of other work.
    let tasks = engine
        .store
        .list_records("orch_task", Some(&t.project))
        .unwrap();
    let sample = tasks[0].clone();
    for n in 0..3000 {
        let mut v = sample.clone();
        v["id"] = json!(format!("bulk-{n}"));
        v["origin"] = json!({ "by": "external" });
        engine
            .store
            .put_record("orch_task", &format!("bulk-{n}"), &t.project, &v)
            .unwrap();
    }
    let at = std::time::Instant::now();
    let _ = lines(engine, &t.project, "ceo", None).unwrap();
    let took = at.elapsed();
    eprintln!("lines over 3000 tasks: {took:?}");
    assert!(took < Duration::from_secs(2), "{took:?}");
}

#[tokio::test]
async fn secrets_in_a_note_a_brief_and_a_reply_are_masked_stored_and_shown() {
    let key = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWx";
    let url = "postgres://admin:hunter2pass@db.internal/prod";
    let t = team(vec![
        (
            "ceo",
            vec![
                note("cmo", &format!("use {key} and {url}")),
                start("cmo", &format!("connect to {url}")),
                answer("asked"),
            ],
        ),
        ("cmo", vec![answer(&format!("done with {key}"))]),
    ])
    .await;
    t.o.run("ceo", said("go")).await.unwrap();
    settled(&t.o, "cmo").await;
    let engine = t.ark.engine();
    let stored: Vec<String> = engine
        .store
        .list_records("orch_message", Some(&t.project))
        .unwrap()
        .iter()
        .map(|v| v.to_string())
        .collect();
    assert!(!stored.is_empty());
    for record in &stored {
        assert!(
            !record.contains("AbCdEfGh") && !record.contains("hunter2pass"),
            "stored: {record}"
        );
    }
    let pair = talk(engine, &t.project, "ceo", "cmo").unwrap();
    let shown: String = pair
        .messages
        .iter()
        .map(|m| m.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !shown.contains("AbCdEfGh") && !shown.contains("hunter2pass"),
        "shown: {shown}"
    );
    assert!(
        shown.contains("postgres://admin:[redacted]@db.internal/prod"),
        "{shown}"
    );
    assert!(shown.contains("[redacted]"), "{shown}");
    assert_eq!(pair.messages.len(), 3, "the words around the secrets stay");
}

#[tokio::test]
async fn an_agent_of_another_project_is_refused_by_talk() {
    let t = team(vec![]).await;
    let other = t.ark.create_project("other", "another").unwrap();
    let o2 = Orchestrator::builder(&t.ark, &other)
        .configure_models(|b| b.provider("m", ByAgent::new(vec![]) as Arc<dyn Provider>))
        .build()
        .unwrap();
    o2.spawn(AgentSpec::new("stranger").model("m"))
        .await
        .unwrap();
    let engine = t.ark.engine();
    assert!(talk(engine, &t.project, "ceo", "stranger").is_err());
    assert!(talk(engine, other.id.as_str(), "stranger", "ceo").is_err());
    assert!(
        talk(engine, other.id.as_str(), "stranger", "stranger").is_ok(),
        "its own project reads"
    );
    assert!(lines(engine, &t.project, "stranger", None)
        .unwrap()
        .is_empty());
}
