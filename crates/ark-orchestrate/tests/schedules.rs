//! Schedules (ADR 0017, 0025, 0053): times, pause and resume, changes, run now, a run never made
//! twice, retries that continue, missed runs, overlap, failures in a row, kept runs, and the
//! chat's words for all of it. Scripted models; no model or network is needed.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ark_model::{Completion, Message, Provider, ProviderError, ToolCall, ToolDef, Usage};
use ark_orchestrate::*;
use ark_sdk::prelude::*;
use serde_json::json;
use tokio::sync::Notify;

/// A scripted model: each call takes the next reply (or error), records the request and the
/// tools it was offered, and may wait for a gate first.
struct Script {
    replies: Mutex<VecDeque<Result<Completion, ProviderError>>>,
    seen: Mutex<Vec<String>>,
    tools: Mutex<Vec<Vec<String>>>,
    gate: Option<Arc<Notify>>,
}

impl Script {
    fn new(replies: Vec<Result<Completion, ProviderError>>) -> Arc<Self> {
        Self::gated(replies, None)
    }
    fn gated(
        replies: Vec<Result<Completion, ProviderError>>,
        gate: Option<Arc<Notify>>,
    ) -> Arc<Self> {
        Arc::new(Script {
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
            tools: Mutex::new(Vec::new()),
            gate,
        })
    }
    fn push(&self, r: Vec<Result<Completion, ProviderError>>) {
        self.replies.lock().unwrap().extend(r);
    }
    fn texts(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Provider for Script {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let mut t = system.to_string();
        for m in messages {
            t.push('\n');
            t.push_str(&m.content);
        }
        self.seen.lock().unwrap().push(t);
        self.tools
            .lock()
            .unwrap()
            .push(tools.iter().map(|d| d.name.clone()).collect());
        if let Some(g) = &self.gate {
            g.notified().await;
        }
        let next = self.replies.lock().unwrap().pop_front();
        next.unwrap_or_else(|| Ok(answer("(no more script)")))
    }
    fn describe(&self) -> String {
        "script".into()
    }
}

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(100, 20),
    }
}

fn call(name: &str, args: serde_json::Value) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c0".into(),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::measured(100, 20),
    }
}

/// A failure the model layer does not retry and a later try may not hit.
fn broke() -> ProviderError {
    ProviderError::Api {
        provider: "script".into(),
        detail: "the model went away".into(),
    }
}

fn no_credit() -> ProviderError {
    ProviderError::OutOfCredit {
        provider: "script".into(),
        detail: "no credit left".into(),
    }
}

/// A state-changing tool that counts the changes it really makes.
struct Doc(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Adapter for Doc {
    fn name(&self) -> &str {
        "doc"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("doc.save", "save", RiskTier::Mutate)]
    }
    async fn run(
        &self,
        _: &str,
        input: serde_json::Value,
        _: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ActionOutput::json(json!({ "saved": input["text"] })))
    }
}

/// An app over one store; each `orch()` is what a new process builds over it.
struct App {
    _dir: tempfile::TempDir,
    ark: Ark,
    project: Project,
    saves: Arc<AtomicUsize>,
}

impl App {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let saves = Arc::new(AtomicUsize::new(0));
        let ark = Ark::builder()
            .store_root(dir.path())
            .tool(Arc::new(Doc(saves.clone())))
            .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("p", "").unwrap();
        App {
            _dir: dir,
            ark,
            project,
            saves,
        }
    }
    fn orch(&self, m: &Arc<Script>) -> Orchestrator {
        let mm = m.clone();
        Orchestrator::builder(&self.ark, &self.project)
            .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
            .build()
            .unwrap()
    }
    async fn with_agent(&self, m: &Arc<Script>) -> Orchestrator {
        let o = self.orch(m);
        o.spawn(
            AgentSpec::new("w")
                .model("m")
                .tools(["doc.*"])
                .allow_mutate(true),
        )
        .await
        .unwrap();
        o
    }
}

fn utc(w: When) -> When {
    w.in_zone("UTC")
}

#[tokio::test]
async fn a_schedule_pauses_resumes_changes_and_runs_now_and_a_pause_survives_a_restart() {
    let app = App::new();
    let gate = Arc::new(Notify::new());
    let m = Script::gated(vec![Ok(answer("counted"))], Some(gate.clone()));
    let o = app.with_agent(&m).await;
    let s = o
        .schedule(
            "w",
            TaskSpec::new("recount the stock"),
            utc(When::daily_at("02:00")),
        )
        .unwrap();
    assert_eq!(s.says, "every day at 02:00");
    let at = s.next_ms.unwrap();

    let p = o.pause_schedule(&s.id).unwrap();
    assert!(p.paused && p.next_ms.is_none());
    assert_eq!(o.schedule_state(&p), ScheduleState::Paused);
    assert!(
        o.tick_schedules(at).is_empty(),
        "a paused schedule runs nothing"
    );
    assert_eq!(o.graph().schedules.len(), 1, "it is kept");
    drop(o);
    let o = app.orch(&m);
    let kept = o.schedules();
    assert_eq!(kept.len(), 1);
    assert!(kept[0].paused, "a pause survives a restart");

    let r = o.resume_schedule(&s.id).unwrap();
    assert!(!r.paused && r.next_ms.is_some());
    assert_eq!(o.schedule_state(&r), ScheduleState::Scheduled);

    let c = o
        .change_schedule(
            &s.id,
            ScheduleChange {
                objective: Some("recount the stock and the returns".into()),
                when: Some(When::daily_at("03:30")),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(c.says, "every day at 03:30");
    assert_eq!(
        c.when.tz.as_deref(),
        Some("UTC"),
        "a new time keeps the zone"
    );
    assert_eq!(c.task.objective, "recount the stock and the returns");
    // A time that passed is refused, and nothing changes.
    let e = o
        .change_schedule(
            &s.id,
            ScheduleChange {
                when: Some(When::on_date("2020-01-01", "09:00")),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(e.to_string().contains("has passed"), "{e}");
    assert_eq!(o.schedules()[0].says, "every day at 03:30");

    let next = o.schedules()[0].next_ms;
    let run = o.run_schedule_now(&s.id).unwrap();
    let after = o.schedules()[0].clone();
    assert_eq!(after.next_ms, next, "run now keeps the next time");
    assert_eq!(after.last_task.as_deref(), Some(run.as_str()));
    // One at a time: a second run now is refused while the first is open.
    let e = o.run_schedule_now(&s.id).unwrap_err();
    assert!(e.to_string().contains("still under way"), "{e}");
    assert_eq!(o.schedule_state(&after), ScheduleState::Running);
    gate.notify_one();
    let done = o.wait(&run).await.unwrap();
    assert_eq!(done.state, TaskState::Done);
    assert!(
        done.context.contains("This is the run scheduled for"),
        "{}",
        done.context
    );
    assert_eq!(
        o.schedule_state(&o.schedules()[0]),
        ScheduleState::Succeeded
    );
    assert_eq!(o.schedule_runs(&s.id, 10).len(), 1);
    let kinds: Vec<_> = o
        .events(0, 500)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::ScheduleChanged { paused, .. } => Some(paused),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec![true, false, false], "pause, resume, change");
}

#[tokio::test]
async fn a_run_started_before_a_crash_is_not_started_again() {
    let app = App::new();
    let m = Script::new(vec![Ok(answer("once"))]);
    let o = app.with_agent(&m).await;
    let s = o
        .schedule("w", TaskSpec::new("poll"), When::every_minutes(10))
        .unwrap();
    let slot = s.next_ms.unwrap();
    drop(o);
    // The process made the run for this time and died before it saved the schedule.
    app.ark
        .engine()
        .store
        .put_record(
            "orch_task",
            "t-made0001",
            app.project.id.as_str(),
            &json!({
                "id": "t-made0001", "project": app.project.id.to_string(), "owner": "w",
                "objective": "poll", "origin": { "by": "external" }, "state": "done",
                "result": "done before", "created_ms": slot, "finished_ms": slot,
                "schedule": s.id, "slot": slot
            }),
        )
        .unwrap();
    let o = app.orch(&m);
    assert!(o.tick_schedules(slot + 1_000).is_empty(), "not made again");
    let after = o.schedules()[0].clone();
    assert_eq!(after.runs, 1);
    assert_eq!(after.last_task.as_deref(), Some("t-made0001"));
    assert_eq!(
        after.next_ms,
        Some(slot + 10 * 60_000),
        "an interval does not drift"
    );
    assert_eq!(m.texts().len(), 0);
}

#[tokio::test]
async fn a_failure_that_may_pass_tries_again_and_continues_without_repeating_a_change() {
    let app = App::new();
    let m = Script::new(vec![
        Ok(call("doc.save", json!({"text": "count"}))),
        Err(broke()),
        // The next try: the model saves the same thing again, then answers.
        Ok(call("doc.save", json!({"text": "count"}))),
        Ok(answer("saved the count")),
    ]);
    let o = app.with_agent(&m).await;
    let t = o
        .submit(
            "w",
            TaskSpec::new("save the count").attempts(2).retry_after(20),
        )
        .unwrap();
    let done = o.wait(&t).await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    assert_eq!(done.attempts, 2);
    assert_eq!(
        app.saves.load(Ordering::SeqCst),
        1,
        "the change is not made twice"
    );
    let texts = m.texts();
    assert!(
        texts[2].contains("you already did, do not do again: doc.save"),
        "{}",
        texts[2]
    );

    // A failure no try can fix is not tried again.
    m.push(vec![Err(no_credit())]);
    let t = o
        .submit("w", TaskSpec::new("again").attempts(3).retry_after(20))
        .unwrap();
    let done = o.wait(&t).await.unwrap();
    assert_eq!((done.state, done.attempts), (TaskState::Failed, 1));
}

#[tokio::test]
async fn a_scheduled_run_waits_to_try_again_and_the_wait_survives_a_restart() {
    let app = App::new();
    let m = Script::new(vec![Err(broke())]);
    let o = app.with_agent(&m).await;
    let s = o
        .schedule("w", TaskSpec::new("poll"), When::every_minutes(10))
        .unwrap();
    let run = o.tick_schedules(s.next_ms.unwrap()).remove(0);
    let rec = o.task(&run).unwrap();
    assert_eq!(rec.max_attempts, 1 + schedule::DEFAULT_RETRIES);
    assert_eq!(rec.retry_delay_ms, Some(schedule::RETRY_DELAY_MS));
    // The first try fails; the run waits five minutes, and says so.
    let mut waited = None;
    for _ in 0..200 {
        let t = o.task(&run).unwrap();
        if t.retry_at_ms.is_some() {
            waited = Some(t);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let t = waited.expect("waits to try again");
    assert_eq!(t.state, TaskState::Queued);
    assert!(
        t.waiting_on
            .as_deref()
            .unwrap_or("")
            .starts_with("trying again at"),
        "{:?}",
        t.waiting_on
    );
    assert_eq!(o.schedule_state(&o.schedules()[0]), ScheduleState::Retrying);
    // A restart keeps the wait: nothing runs at once.
    drop(o);
    let o = app.orch(&m);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let t = o.task(&run).unwrap();
    assert!(t.retry_at_ms.is_some() && !t.state.is_final());
    assert_eq!(m.texts().len(), 1, "no second try yet");
    // Pausing the schedule cancels a run that only waits to try again.
    o.pause_schedule(&o.schedules()[0].id).unwrap();
    assert_eq!(o.wait(&run).await.unwrap().state, TaskState::Cancelled);
}

#[tokio::test]
async fn a_missed_run_runs_once_or_is_let_go_and_overlap_queues_one_more() {
    let app = App::new();
    let gate = Arc::new(Notify::new());
    let m = Script::gated(vec![], Some(gate.clone()));
    let o = app.with_agent(&m).await;
    let once = o
        .schedule("w", TaskSpec::new("report"), When::every_minutes(10))
        .unwrap();
    let skip = o
        .schedule_with(
            "w",
            TaskSpec::new("poll"),
            When::every_minutes(10),
            ScheduleChange {
                if_missed: Some(IfMissed::Skip),
                ..Default::default()
            },
        )
        .unwrap();
    // The computer was off for two hours.
    let late = once.next_ms.unwrap().max(skip.next_ms.unwrap()) + 2 * 3_600_000;
    let started = o.tick_schedules(late);
    assert_eq!(started.len(), 1, "only the one that runs missed times");
    let s = o.schedule_record(&skip.id).unwrap();
    assert_eq!((s.runs, s.missed), (0, 1));
    assert!(s.next_ms.unwrap() > late);
    let first = o.task(&started[0]).unwrap();
    assert!(
        first.context.contains("It starts late"),
        "{}",
        first.context
    );

    // Overlap: one more run waits behind the open one; a third is skipped.
    let both = o
        .schedule_with(
            "w",
            TaskSpec::new("sync"),
            When::every_minutes(10),
            ScheduleChange {
                overlap: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let t0 = both.next_ms.unwrap();
    assert_eq!(o.tick_schedules(t0).len(), 1);
    assert_eq!(o.tick_schedules(t0 + 10 * 60_000).len(), 1, "queues behind");
    assert!(o.tick_schedules(t0 + 20 * 60_000).is_empty(), "not a third");
    assert_eq!(o.schedule_record(&both.id).unwrap().skipped, 1);
    for _ in 0..4 {
        gate.notify_one();
    }
}

#[tokio::test]
async fn three_failed_runs_in_a_row_pause_the_schedule_and_the_chat_is_told() {
    let app = App::new();
    let m = Script::new(vec![Err(no_credit()), Err(no_credit()), Err(no_credit())]);
    let o = app.with_agent(&m).await;
    let session = app.ark.start_session(&app.project.id).unwrap();
    let chat = session.id().as_str().to_string();
    let s = o
        .schedule(
            "w",
            TaskSpec::new("poll").report_to(&chat),
            When::every_minutes(10),
        )
        .unwrap();
    let mut at = s.next_ms.unwrap();
    for _ in 0..3 {
        let run = o.tick_schedules(at);
        assert_eq!(run.len(), 1);
        assert_eq!(o.wait(&run[0]).await.unwrap().state, TaskState::Failed);
        at += 10 * 60_000;
    }
    // Paused when the third ends, not at the next time.
    let _ = at;
    let p = o.schedules()[0].clone();
    assert!(p.paused, "{p:?}");
    let posts = ark_runtime::session::posts(app.ark.engine(), &chat, 0, 10).unwrap();
    assert!(
        posts
            .iter()
            .any(|(_, p)| p.text.starts_with("Paused after 3 runs")),
        "{posts:?}"
    );
    // Resumed, it counts again from zero.
    assert_eq!(o.resume_schedule(&s.id).unwrap().failures, 0);
}

#[tokio::test]
async fn a_schedule_keeps_its_newest_runs() {
    let app = App::new();
    let m = Script::new(vec![]);
    let o = app.with_agent(&m).await;
    let s = o
        .schedule("w", TaskSpec::new("poll"), When::every_minutes(5))
        .unwrap();
    let mut at = s.next_ms.unwrap();
    let mut first = None;
    for _ in 0..53 {
        let run = o.tick_schedules(at).remove(0);
        first.get_or_insert(run.clone());
        o.wait(&run).await.unwrap();
        at += 5 * 60_000;
    }
    let runs = o.schedule_runs(&s.id, 100);
    assert!(runs.len() <= 51, "{}", runs.len());
    assert!(
        o.task(first.as_ref().unwrap()).is_none(),
        "the oldest left the store"
    );
    assert!(app
        .ark
        .engine()
        .store
        .get_record("orch_task", first.as_ref().unwrap())
        .unwrap()
        .is_none());
}

/// The person's conversation on one model, with the schedule tools.
async fn chat(
    chat: &Arc<Script>,
    zone: Option<&str>,
) -> (tempfile::TempDir, Ark, Orchestrator, Session) {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(chat.clone() as Arc<dyn Provider>)
        .allow_mutate(true)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("shop", "").unwrap();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    let session = ark.start_session(&project.id).unwrap();
    let _ = zone;
    (dir, ark, o, session)
}

async fn say(ark: &Ark, o: &Orchestrator, s: &mut Session, zone: Option<&str>, text: &str) {
    let id = s.id().as_str().to_string();
    let opts = TurnOptions {
        tools: vec![o.chat_tools_in_zone(&id, zone)],
        ..Default::default()
    };
    ark.ask_session(s, text, opts, |_| {}).await.unwrap();
}

#[tokio::test]
async fn the_chat_schedules_without_naming_an_agent_and_manages_it_in_words() {
    let m = Script::new(vec![
        Ok(call(
            "agents.schedule",
            json!({"objective": "recount the stock in stock.xlsx and summarize it", "at": "02:00"}),
        )),
        Ok(answer("Every day at 02:00 [inv:1]")),
    ]);
    let (_dir, ark, o, mut s) = chat(&m, Some("Asia/Tokyo")).await;
    say(
        &ark,
        &o,
        &mut s,
        Some("Asia/Tokyo"),
        "every day at 2 AM recount the stock",
    )
    .await;
    // The model was told the date and time now, in the person's zone.
    assert!(
        m.texts()[0].contains("Today: "),
        "the description dates the request"
    );
    assert!(m.texts()[0].contains("(Asia/Tokyo)"));
    let all = o.schedules();
    assert_eq!(all.len(), 1);
    let sched = all[0].clone();
    assert_eq!(sched.when.tz.as_deref(), Some("Asia/Tokyo"));
    assert_eq!(sched.agent, "recount-the-stock", "its own agent");
    assert_eq!(sched.task.report_to.as_deref(), Some(s.id().as_str()));
    // Making or changing standing work is asked every time.
    let tools = o.chat_tools_for(s.id().as_str());
    let d = tools
        .adapter
        .actions()
        .into_iter()
        .find(|a| a.name == "agents.schedule")
        .unwrap();
    assert!(d.ask_each_time);
    let stop = tools
        .adapter
        .actions()
        .into_iter()
        .find(|a| a.name == "agents.unschedule")
        .unwrap();
    assert!(stop.summary.contains(&sched.id), "lists what is scheduled");

    m.push(vec![
        Ok(call(
            "agents.schedule",
            json!({"schedule": sched.id, "at": "03:00"}),
        )),
        Ok(answer("moved")),
        Ok(call(
            "agents.unschedule",
            json!({"schedule": sched.id, "pause": true}),
        )),
        Ok(answer("paused")),
        Ok(call(
            "agents.schedule",
            json!({"schedule": sched.id, "resume": true, "run_now": true}),
        )),
        Ok(answer("running")),
    ]);
    // From a client in another zone: the schedule keeps its own.
    say(&ark, &o, &mut s, Some("Europe/Madrid"), "make it 3 AM").await;
    assert_eq!(o.schedules()[0].says, "every day at 03:00");
    assert_eq!(o.schedules()[0].when.tz.as_deref(), Some("Asia/Tokyo"));
    say(&ark, &o, &mut s, Some("Asia/Tokyo"), "pause it").await;
    assert!(o.schedules()[0].paused);
    say(
        &ark,
        &o,
        &mut s,
        Some("Asia/Tokyo"),
        "resume it and run it now",
    )
    .await;
    let now = o.schedules()[0].clone();
    assert!(!now.paused && now.last_task.is_some());
    let run = o.wait(now.last_task.as_ref().unwrap()).await.unwrap();
    // The run's agent was never offered the schedule tools: only the chat makes standing work.
    let offered = m.tools.lock().unwrap().clone();
    let runs_tools = offered.last().unwrap();
    assert!(
        !runs_tools
            .iter()
            .any(|t| matches!(t.as_str(), "agents.schedule" | "agents.unschedule")),
        "{runs_tools:?}"
    );
    assert_eq!(run.state, TaskState::Done);
    // Stopped: gone, its run stays.
    m.push(vec![
        Ok(call("agents.unschedule", json!({"schedule": sched.id}))),
        Ok(answer("stopped")),
    ]);
    say(&ark, &o, &mut s, Some("Asia/Tokyo"), "stop it").await;
    assert!(o.schedules().is_empty());
    assert!(o.task(&run.id).is_some());
    // Its own agent ends with it.
    assert_eq!(
        o.agent("recount-the-stock").unwrap().state,
        AgentState::Terminated
    );
}

#[tokio::test]
async fn a_date_that_passed_or_a_bad_time_is_refused_with_words_to_fix_it() {
    let m = Script::new(vec![
        Ok(call(
            "agents.schedule",
            json!({"objective": "send the report", "at": "09:00", "date": "2020-01-01"}),
        )),
        Ok(call(
            "agents.schedule",
            json!({"objective": "send the report", "cron": "* * * * *"}),
        )),
        Ok(answer("could not")),
    ]);
    let (_dir, ark, o, mut s) = chat(&m, None).await;
    say(&ark, &o, &mut s, None, "on 1 Jan 2020 send the report").await;
    let texts = m.texts();
    assert!(texts[1].contains("has passed"), "{}", texts[1]);
    assert!(
        texts[2].contains("more often than every 5 minutes"),
        "{}",
        texts[2]
    );
    assert!(o.schedules().is_empty());
}

#[tokio::test]
async fn an_unknown_schedule_is_refused_at_once() {
    let app = App::new();
    let m = Script::new(vec![]);
    let o = app.with_agent(&m).await;
    o.schedule("w", TaskSpec::new("poll"), When::every_minutes(10))
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let oo = o.clone();
    std::thread::spawn(move || {
        let errs = [
            oo.pause_schedule("s-nope").unwrap_err().to_string(),
            oo.resume_schedule("s-nope").unwrap_err().to_string(),
            oo.change_schedule("s-nope", ScheduleChange::default())
                .unwrap_err()
                .to_string(),
            oo.run_schedule_now("s-nope").unwrap_err().to_string(),
            oo.unschedule("s-nope").unwrap_err().to_string(),
        ];
        tx.send(errs).unwrap();
    });
    let errs = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("answered, not stuck");
    for e in errs {
        assert!(
            e.contains("no schedule `s-nope`; the schedules are s-"),
            "{e}"
        );
    }
    // The timer still runs.
    let next = o.schedules()[0].next_ms.unwrap();
    assert_eq!(o.tick_schedules(next).len(), 1);
}

#[tokio::test]
async fn a_one_time_schedule_stays_until_its_run_ends() {
    let app = App::new();
    let gate = Arc::new(Notify::new());
    let m = Script::gated(vec![Ok(answer("sent"))], Some(gate.clone()));
    let o = app.with_agent(&m).await;
    let s = o
        .schedule(
            "w",
            TaskSpec::new("send the report"),
            When::every_minutes(10).once(),
        )
        .unwrap();
    let run = o.tick_schedules(s.next_ms.unwrap()).remove(0);
    let kept = o.schedules();
    assert_eq!(kept.len(), 1, "shown while its run works");
    assert_eq!(o.schedule_state(&kept[0]), ScheduleState::Running);
    gate.notify_one();
    assert_eq!(o.wait(&run).await.unwrap().state, TaskState::Done);
    assert!(o.schedules().is_empty(), "gone when its run ended");
    assert!(app
        .ark
        .engine()
        .store
        .get_record("orch_schedule", &s.id)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_one_time_schedule_whose_run_a_stopped_process_left_open_ends_with_that_run() {
    let app = App::new();
    let m = Script::new(vec![Ok(answer("sent after the restart"))]);
    let o = app.with_agent(&m).await;
    let s = o
        .schedule(
            "w",
            TaskSpec::new("send the report"),
            When::every_minutes(10).once(),
        )
        .unwrap();
    drop(o);
    // A process that is gone started the run and ended the schedule's times.
    let store = &app.ark.engine().store;
    let mut rec = store.get_record("orch_schedule", &s.id).unwrap().unwrap();
    rec["next_ms"] = json!(null);
    rec["last_task"] = json!("t-left0001");
    rec["runs"] = json!(1);
    store
        .put_record("orch_schedule", &s.id, app.project.id.as_str(), &rec)
        .unwrap();
    store
        .put_record(
            "orch_task",
            "t-left0001",
            app.project.id.as_str(),
            &json!({
                "id": "t-left0001", "project": app.project.id.to_string(), "owner": "w",
                "objective": "send the report", "origin": { "by": "external" }, "state": "queued",
                "created_ms": 1, "schedule": s.id, "slot": 1, "pid": 999_999_u32
            }),
        )
        .unwrap();
    let o = app.orch(&m);
    let done = o.wait("t-left0001").await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    assert!(o.schedules().is_empty(), "gone when its run ended");
}
