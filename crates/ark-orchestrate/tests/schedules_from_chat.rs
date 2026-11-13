//! A named agent schedules work from its own chat and each run answers there as its next message
//! (ADR 0120). Scripted models only.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_core::ExecutionMode;
use ark_model::{Completion, Message, Provider, ProviderError, ToolCall, ToolDef, Usage};
use ark_orchestrate::*;
use ark_runtime::session::{posts, Post, PostKind, Session};
use ark_sdk::prelude::*;
use serde_json::{json, Value};

const OFFERED: [&str; 5] = [
    "agents.schedule",
    "agents.unschedule",
    "work.later",
    "groups.set",
    "role.set",
];

/// Answers by what the request holds. The person's message holds a marker: "zzmake" schedules
/// a daily run (its objective carries "zzrun", and "zzkey" or "zzfail" when the message has
/// them), "zzfast" asks for one every 5 minutes, "zzfor <agent>" schedules for that agent,
/// "zzstop s-..." stops that schedule, "zzkick" starts a task for `bob`. A request with "zzrun"
/// and no "zzmake" is a scheduled run.
struct Model {
    requests: Mutex<Vec<(String, String)>>,
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

#[async_trait::async_trait]
impl Provider for Model {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let text = format!(
            "{system}\n{}",
            messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        // What the model is told it can use: the capability index in the system prompt and the
        // tools loaded into the request.
        self.requests.lock().unwrap().push((
            text.clone(),
            format!(
                "{system}\n{}",
                tools
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ));
        let used = |name: &str| {
            messages
                .iter()
                .any(|m| m.tool_name.as_deref() == Some(name))
        };
        let word_after = |marker: &str| {
            text.split_once(marker)
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .unwrap_or("")
                .to_string()
        };
        let marked = |m: &str| text.contains(m);
        if marked("zzmake") || marked("zzfast") || marked("zzfor") {
            if used("agents.schedule") {
                return Ok(answer("Scheduled."));
            }
            let mut objective = "Check the stock zzrun".to_string();
            for marker in ["zzkey", "zzfail"] {
                if marked(marker) {
                    objective = format!("{objective} {marker}");
                }
            }
            let mut args = json!({ "objective": objective, "at": "09:00" });
            if marked("zzfast") {
                args = json!({ "objective": objective, "every_minutes": 5 });
            }
            if marked("zzfor") {
                args["to"] = json!(word_after("zzfor "));
            }
            return Ok(call("agents.schedule", args));
        }
        if marked("zzstop") {
            if used("agents.unschedule") {
                return Ok(answer("Stopped."));
            }
            return Ok(call(
                "agents.unschedule",
                json!({ "schedule": word_after("zzstop ") }),
            ));
        }
        if marked("zzkick") {
            if used("agents.start") {
                return Ok(answer("started"));
            }
            return Ok(call(
                "agents.start",
                json!({ "to": "bob", "objective": "assist bob" }),
            ));
        }
        if marked("zzrun") {
            if marked("zzfail") {
                return Err(ProviderError::OutOfCredit {
                    provider: "m".into(),
                    detail: "no credit left".into(),
                });
            }
            if marked("zzkey") {
                return Ok(answer("Stock is 42. The key is sk-ant-api03-abcdefghij"));
            }
            return Ok(answer("Stock is 42."));
        }
        Ok(answer("done"))
    }

    fn describe(&self) -> String {
        "m".into()
    }
}

/// Counts the questions and answers each with `allow`.
struct Decide(Arc<AtomicUsize>, bool);

#[async_trait::async_trait]
impl Approver for Decide {
    async fn decide(&self, _: &ApprovalRequest) -> ApprovalDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        if self.1 {
            ApprovalDecision::AllowOnce
        } else {
            ApprovalDecision::Deny
        }
    }
}

struct World {
    _dir: tempfile::TempDir,
    ark: Ark,
    o: Orchestrator,
    m: Arc<Model>,
    asked: Arc<AtomicUsize>,
}

fn world(allow: bool) -> World {
    let m = Arc::new(Model {
        requests: Mutex::new(Vec::new()),
    });
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let asked = Arc::new(AtomicUsize::new(0));
    let mm = m.clone();
    let o = Orchestrator::builder(&ark, &project)
        .approvals(Arc::new(ApprovalGate::new(Arc::new(Decide(
            asked.clone(),
            allow,
        )))))
        .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
        .build()
        .unwrap();
    World {
        _dir: dir,
        ark,
        o,
        m,
        asked,
    }
}

fn said(text: &str) -> TaskSpec {
    let mut t = TaskSpec::new(text);
    t.person = true;
    t
}

async fn agent(w: &World, name: &str, spec: impl FnOnce(AgentSpec) -> AgentSpec) {
    w.o.spawn(spec(AgentSpec::new(name).model("m")))
        .await
        .unwrap();
}

/// A new conversation of `agent` whose mode the person chose.
fn chat_in(w: &World, agent: &str, mode: ExecutionMode) -> String {
    let id = w.o.start_thread(agent).unwrap();
    let mut s = Session::open(w.ark.engine(), &id).unwrap().unwrap();
    s.set_execution_mode(w.ark.engine(), s.execution_mode_revision(), mode)
        .unwrap();
    id
}

/// A turn of `agent` in a conversation of its own, so no earlier turn's calls are in it.
async fn say_in_new_chat(w: &World, agent: &str, text: &str) -> TaskRecord {
    let id = w.o.start_thread(agent).unwrap();
    w.o.run(agent, said(text).thread(&id)).await.unwrap()
}

fn chat_posts(w: &World, chat: &str) -> Vec<Post> {
    posts(w.ark.engine(), chat, 0, 50)
        .unwrap()
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..500 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never: {what}");
}

/// What the first request that holds `marker` and not `without` tells the model it can use.
fn offered(w: &World, marker: &str, without: &str) -> String {
    w.m.requests
        .lock()
        .unwrap()
        .iter()
        .find(|(t, _)| t.contains(marker) && (without.is_empty() || !t.contains(without)))
        .unwrap_or_else(|| panic!("no request with {marker}"))
        .1
        .clone()
}

#[tokio::test]
async fn in_auto_it_schedules_without_a_question_and_each_run_answers_as_a_message_once() {
    let w = world(false);
    agent(&w, "ceo", |s| s).await;
    let turn =
        w.o.run("ceo", said("zzmake zzkey every morning"))
            .await
            .unwrap();
    assert_eq!(turn.state, TaskState::Done);
    assert_eq!(w.asked.load(Ordering::SeqCst), 0, "Auto asks nothing");
    let all = w.o.schedules();
    assert_eq!(all.len(), 1);
    let sched = all[0].clone();
    let chat = w.o.agent("ceo").unwrap().session.expect("its chat");
    assert_eq!(sched.task.report_to.as_deref(), Some(chat.as_str()));
    assert!(sched.task.reply, "each run is the agent's message");
    assert!(sched.own_agent);
    let on_turn = offered(&w, "zzmake", "");
    assert!(
        on_turn.contains(&"agents.schedule".to_string()),
        "{on_turn:?}"
    );

    // The Calendar reads it like any schedule.
    let now = jiff::Timestamp::now().as_millisecond() as u64;
    let snap = w.o.calendar_snapshot(now, now + 3 * 86_400_000).unwrap();
    assert!(snap.schedules.iter().any(|s| s.id == sched.id));

    let run = w.o.run_schedule_now(&sched.id).await.unwrap();
    until("a post", || !chat_posts(&w, &chat).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let posted = chat_posts(&w, &chat);
    assert_eq!(posted.len(), 1, "once: {posted:?}");
    assert_eq!(posted[0].kind, PostKind::Message);
    assert!(posted[0].ok);
    assert!(
        posted[0].text.starts_with("Stock is 42."),
        "{}",
        posted[0].text
    );
    assert!(
        !posted[0].text.contains("sk-ant-api03"),
        "scrubbed: {}",
        posted[0].text
    );
    assert_eq!(posted[0].work.as_deref(), Some(run.as_str()));

    // The run is not a chat turn: it is offered no work.later, groups.set, role.set or schedule
    // tool, so a schedule never makes schedules.
    let in_run = offered(&w, "zzrun", "zzmake");
    for name in OFFERED {
        assert!(!in_run.contains(&name.to_string()), "{name} in {in_run:?}");
    }
}

#[tokio::test]
async fn in_ask_mode_it_asks_each_time_and_a_no_creates_nothing() {
    let refuse = world(false);
    agent(&refuse, "ceo", |s| s).await;
    let chat = chat_in(&refuse, "ceo", ExecutionMode::Manual);
    refuse
        .o
        .run("ceo", said("zzmake").thread(&chat))
        .await
        .unwrap();
    assert_eq!(refuse.asked.load(Ordering::SeqCst), 1);
    assert!(refuse.o.schedules().is_empty());

    let allow = world(true);
    agent(&allow, "ceo", |s| s).await;
    let chat = chat_in(&allow, "ceo", ExecutionMode::Manual);
    allow
        .o
        .run("ceo", said("zzmake").thread(&chat))
        .await
        .unwrap();
    assert_eq!(allow.asked.load(Ordering::SeqCst), 1);
    let all = allow.o.schedules();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].task.report_to.as_deref(), Some(chat.as_str()));
}

#[tokio::test]
async fn in_plan_mode_the_schedule_tools_are_not_offered() {
    let w = world(true);
    agent(&w, "ceo", |s| s).await;
    let chat = chat_in(&w, "ceo", ExecutionMode::Plan);
    w.o.run("ceo", said("zzping").thread(&chat)).await.unwrap();
    let tools = offered(&w, "zzping", "");
    assert!(!tools.contains(&"agents.schedule".to_string()), "{tools:?}");
    assert!(!tools.contains(&"agents.unschedule".to_string()));
}

#[tokio::test]
async fn an_agent_whose_tools_leave_it_out_is_not_offered_them() {
    let w = world(true);
    agent(&w, "ceo", |s| s.tools(["work.later"])).await;
    w.o.run("ceo", said("zzping")).await.unwrap();
    let tools = offered(&w, "zzping", "");
    assert!(!tools.contains(&"agents.schedule".to_string()), "{tools:?}");
}

#[tokio::test]
async fn the_agent_made_for_a_schedule_has_no_more_tools_than_its_maker_and_asks() {
    let w = world(true);
    agent(&w, "ceo", |s| {
        s.tools(["agents.schedule", "agents.unschedule"])
            .allow_mutate(true)
    })
    .await;
    w.o.run("ceo", said("zzmake")).await.unwrap();
    let own = w.o.schedules()[0].agent.clone();
    let spec = w.o.agent(&own).unwrap().spec;
    assert_eq!(spec.tools, ["agents.schedule", "agents.unschedule"]);
    assert!(!spec.allow_mutate, "its changes ask");
    assert_eq!(spec.parent.as_deref(), Some("ceo"));
}

#[tokio::test]
async fn a_schedule_for_a_group_only_contact_is_refused_and_a_task_it_started_has_no_schedule_tools(
) {
    let w = world(true);
    agent(&w, "ana", |mut s| {
        s.groups = vec!["g".into()];
        s.allow_mutate(true)
    })
    .await;
    agent(&w, "bob", |mut s| {
        s.groups = vec!["g".into()];
        s.allow_mutate(true)
    })
    .await;
    // Ana may not have Bob run standing work: a run has no origin, so it would act alone.
    say_in_new_chat(&w, "ana", "zzfor bob").await;
    assert!(w.o.schedules().is_empty());
    // Nor an agent it delegates to that acts alone: it would run with that agent's authority.
    agent(&w, "dan", |s| s.allow_mutate(true)).await;
    agent(&w, "eve", |s| s.delegates_to(["dan"])).await;
    say_in_new_chat(&w, "eve", "zzfor dan").await;
    assert!(w.o.schedules().is_empty());
    // For herself it is allowed.
    say_in_new_chat(&w, "ana", "zzfor ana").await;
    assert_eq!(w.o.schedules().len(), 1);
    assert_eq!(w.o.schedules()[0].agent, "ana");

    // What Ana starts for Bob through the group is not a chat turn of Bob's.
    say_in_new_chat(&w, "ana", "zzkick").await;
    until("bob ran", || {
        w.o.tasks()
            .iter()
            .any(|t| t.owner == "bob" && t.state.is_final())
    })
    .await;
    let bobs = offered(&w, "assist bob", "zzkick");
    for name in OFFERED {
        assert!(!bobs.contains(&name.to_string()), "{name} in {bobs:?}");
    }
    // A turn the group driver makes for a member is a chat turn: it may schedule for itself.
    let mut turn = TaskSpec::new("zzmake");
    turn.group_turn = true;
    w.o.run("bob", turn).await.unwrap();
    let tools = offered(&w, "zzmake", "");
    assert!(tools.contains(&"agents.schedule".to_string()), "{tools:?}");
    let chat = w.o.agent("bob").unwrap().session.unwrap();
    assert!(w
        .o
        .schedules()
        .iter()
        .any(|s| s.task.reply && s.task.report_to.as_deref() == Some(chat.as_str())));
}

#[tokio::test]
async fn it_sees_and_stops_only_its_own_schedules_and_keeps_at_most_ten() {
    let w = world(true);
    agent(&w, "ceo", |s| s).await;
    agent(&w, "dan", |s| s).await;
    let other = w.ark.start_session(&w.o.project().id).unwrap();
    let theirs =
        w.o.schedule(
            "dan",
            TaskSpec::new("dan's report").report_to(other.id().as_str()),
            When::every_minutes(120),
        )
        .unwrap();
    // Not Ceo's: refused, still there.
    say_in_new_chat(&w, "ceo", &format!("zzstop {}", theirs.id)).await;
    assert_eq!(
        w.o.schedules().len(),
        1,
        "dan's schedule is not Ceo's to stop"
    );
    // Ceo's own can be stopped, and its agent ends with it.
    say_in_new_chat(&w, "ceo", "zzmake").await;
    let mine =
        w.o.schedules()
            .into_iter()
            .find(|s| s.id != theirs.id)
            .unwrap();
    say_in_new_chat(&w, "ceo", &format!("zzstop {}", mine.id)).await;
    assert_eq!(w.o.schedules().len(), 1);
    assert_eq!(
        w.o.agent(&mine.agent).unwrap().state,
        AgentState::Terminated
    );
    // At most ten.
    for _ in 0..12 {
        say_in_new_chat(&w, "ceo", "zzmake").await;
    }
    let ceos =
        w.o.schedules()
            .into_iter()
            .filter(|s| s.id != theirs.id)
            .count();
    assert_eq!(ceos, 10);
}

#[tokio::test]
async fn runs_set_from_a_chat_are_at_most_once_an_hour() {
    let w = world(true);
    agent(&w, "ceo", |s| s).await;
    w.o.run("ceo", said("zzfast")).await.unwrap();
    assert!(w.o.schedules().is_empty());
}

#[tokio::test]
async fn three_failed_runs_pause_it_and_the_chat_is_told() {
    let w = world(true);
    agent(&w, "ceo", |s| s).await;
    w.o.run("ceo", said("zzmake zzfail")).await.unwrap();
    let chat = w.o.agent("ceo").unwrap().session.unwrap();
    let id = w.o.schedules()[0].id.clone();
    for _ in 0..3 {
        let at = w.o.schedules()[0].next_ms.unwrap();
        let run = w.o.tick_schedules(at).await;
        assert_eq!(run.len(), 1);
        assert_eq!(w.o.wait(&run[0]).await.unwrap().state, TaskState::Failed);
    }
    until("paused", || w.o.schedule_record(&id).unwrap().paused).await;
    let all = chat_posts(&w, &chat);
    let failed: Vec<_> = all
        .iter()
        .filter(|p| p.kind == PostKind::Message && !p.ok)
        .collect();
    assert_eq!(failed.len(), 3, "each failed run says so: {all:?}");
    assert!(
        all.iter()
            .any(|p| p.text.starts_with("Paused after 3 runs")),
        "{all:?}"
    );
}

#[tokio::test]
async fn an_agent_that_acts_alone_does_not_ask_in_ask_mode() {
    let w = world(false);
    agent(&w, "ceo", |s| s.allow_mutate(true)).await;
    let chat = chat_in(&w, "ceo", ExecutionMode::Manual);
    w.o.run("ceo", said("zzmake").thread(&chat)).await.unwrap();
    assert_eq!(w.asked.load(Ordering::SeqCst), 0);
    assert_eq!(w.o.schedules().len(), 1);
}

#[tokio::test]
async fn a_result_chat_of_another_project_is_refused() {
    let w = world(true);
    agent(&w, "ceo", |s| s).await;
    let elsewhere = w.ark.create_project("elsewhere", "").unwrap();
    let foreign = w.ark.start_session(&elsewhere.id).unwrap();
    let s =
        w.o.schedule(
            "ceo",
            TaskSpec::new("report").report_to(foreign.id().as_str()),
            When::every_minutes(120),
        )
        .unwrap();
    let refused = w.o.run_schedule_now(&s.id).await.unwrap_err().to_string();
    assert!(refused.contains("unavailable in this project"), "{refused}");
    assert!(w.o.tasks().is_empty());
}

#[tokio::test]
async fn a_schedule_the_person_made_for_the_agent_is_not_the_agents_to_change_stop_or_run() {
    let w = world(true);
    agent(&w, "ceo", |s| s.allow_mutate(true)).await;
    // The person's own tools may report into the agent's chat.
    let chat = w.o.start_thread("ceo").unwrap();
    let theirs =
        w.o.schedule(
            "ceo",
            TaskSpec::new("the person's report").report_to(&chat),
            When::every_minutes(120),
        )
        .unwrap();
    // The model's call is `agents.unschedule` with this id (zzstop), in that chat.
    w.o.run("ceo", said(&format!("zzstop {}", theirs.id)).thread(&chat))
        .await
        .unwrap();
    let kept = w.o.schedule_record(&theirs.id).expect("still there");
    assert!(!kept.paused);
    assert_eq!(kept.task.objective, "the person's report");
    // And it is not listed in what the model is told it can stop.
    let told = offered(&w, &theirs.id, "");
    assert!(
        !told.contains(&format!("{}: the person", theirs.id)),
        "{told}"
    );
}
