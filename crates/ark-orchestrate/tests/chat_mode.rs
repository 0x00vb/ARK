//! A named agent's chat turn follows the chat's execution mode (ADR 0123). Scripted models only.
//!
//! Every action here is a fake that records that it ran; the approver records what it was asked
//! and refuses, so a card shows as an entry in `asked` and the action as missing from `ran`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_core::ExecutionMode;
use ark_model::{Completion, Message, Provider, Role, ToolCall, ToolDef, Usage};
use ark_orchestrate::*;
use ark_runtime::session::Session;
use ark_sdk::prelude::*;
use serde_json::{json, Value};
use tokio::sync::Notify;

const BRIEF: &str = "Read the filings and report the numbers";
const REPLAY: [&str; 3] = ["process.start", "files.write", "files.edit"];

/// What the request's own message says to do. "zzreplay" runs the owner's sequence, "zzask"
/// runs a plain change, one that asks every time and one only the person may run, "zzhand"
/// hands "zzrun" work off with `work.later`, "zzrun" is that work (it starts a process),
/// "zzdelegate" and "zzkick" send "zzreplay" work to the helper, "zzblock" waits.
struct Model {
    block: Notify,
    hold_turn: bool,
    seen: Mutex<Vec<String>>,
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

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

#[async_trait::async_trait]
impl Provider for Model {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        _: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        self.seen.lock().unwrap().push(format!(
            "{system}\n{}",
            messages.last().map_or("", |m| &m.content)
        ));
        let ask = messages.iter().rposition(|m| {
            m.role == Role::User && m.tool_name.is_none() && m.content.contains("zz")
        });
        let Some(ask) = ask else {
            return Ok(answer("done"));
        };
        let said = messages[ask].content.as_str();
        let results = messages.iter().filter(|m| m.tool_name.is_some()).count();
        let step = |plan: &[(&str, Value)]| match plan.get(results) {
            Some((name, args)) => call(name, args.clone()),
            None => answer("finished"),
        };
        // A continuation after started work (ADR 0126) quotes the helper's brief: it answers.
        if said.contains("colleagues you started from this chat have finished") {
            return Ok(answer("combined"));
        }
        if said.contains("zzhand") {
            let brief = json!({ "brief": format!("{BRIEF} zzrun") });
            if results == 1 && self.hold_turn {
                self.block.notified().await;
            }
            return Ok(step(&[("work.later", brief)]));
        }
        if said.contains("zzrun") {
            return Ok(step(&[("process.start", json!({}))]));
        }
        if said.contains("zzblock") {
            self.block.notified().await;
            return Ok(answer("unblocked"));
        }
        if said.contains("zzdelegate") {
            let delegate = json!({ "to": "helper", "objective": "zzreplay delegated" });
            return Ok(step(&[("agents.delegate", delegate)]));
        }
        if said.contains("zzkickscreen") {
            let start = json!({ "to": "helper", "objective": "zzscreen started" });
            return Ok(step(&[("agents.start", start)]));
        }
        if said.contains("zzscreen") {
            return Ok(step(&[
                ("screen.look", json!({})),
                ("screen.click", json!({})),
            ]));
        }
        if said.contains("zzkick") {
            let start = json!({ "to": "helper", "objective": "zzreplay started" });
            return Ok(step(&[("agents.start", start)]));
        }
        if said.contains("zzask") {
            return Ok(step(&[
                ("process.start", json!({})),
                ("computer.look", json!({})),
                ("pay.send", json!({})),
            ]));
        }
        if said.contains("zzreplay") {
            return Ok(step(&[
                ("process.start", json!({})),
                ("files.write", json!({})),
                ("files.edit", json!({})),
            ]));
        }
        Ok(answer("pong"))
    }

    fn describe(&self) -> String {
        "m".into()
    }
}

struct Fake {
    name: &'static str,
    actions: Vec<ActionDescriptor>,
    ran: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Adapter for Fake {
    fn name(&self) -> &str {
        self.name
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        self.actions.clone()
    }
    async fn run(&self, action: &str, _: Value, _: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        self.ran.lock().unwrap().push(action.to_string());
        Ok(ActionOutput::json(json!({ "ok": true })))
    }
}

fn change(name: &str) -> ActionDescriptor {
    ActionDescriptor::new(name, "change", RiskTier::Mutate)
}

/// Records each question and answers no, or never answers when `wait`.
struct Refuse(Arc<Mutex<Vec<String>>>, bool);

#[async_trait::async_trait]
impl Approver for Refuse {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        self.0.lock().unwrap().push(request.action.clone());
        if self.1 {
            std::future::pending::<()>().await;
        }
        ApprovalDecision::Deny
    }
}

struct World {
    _dir: tempfile::TempDir,
    ark: Ark,
    o: Orchestrator,
    m: Arc<Model>,
    ran: Arc<Mutex<Vec<String>>>,
    asked: Arc<Mutex<Vec<String>>>,
}

fn world(hold: bool) -> World {
    world_with(hold, false)
}

fn world_with(hold: bool, wait: bool) -> World {
    let ran = Arc::new(Mutex::new(Vec::new()));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let m = Arc::new(Model {
        block: Notify::new(),
        hold_turn: hold,
        seen: Mutex::new(Vec::new()),
    });
    let dir = tempfile::tempdir().unwrap();
    let fakes = [
        ("process", vec![change("process.start")]),
        ("files", vec![change("files.write"), change("files.edit")]),
        ("computer", vec![change("computer.look").ask_each_time()]),
        (
            "screen",
            vec![
                change("screen.look")
                    .grant_per("screen", &[])
                    .ask_once_a_turn(),
                change("screen.click")
                    .grant_per("screen", &[])
                    .ask_once_a_turn(),
            ],
        ),
        ("pay", vec![change("pay.send").person_only()]),
    ];
    let mut builder = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false);
    for (name, actions) in fakes {
        builder = builder.tool(Arc::new(Fake {
            name,
            actions,
            ran: ran.clone(),
        }));
    }
    let ark = builder.build().unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let mm = m.clone();
    let o = Orchestrator::builder(&ark, &project)
        .max_parallel(if hold { 1 } else { 8 })
        .approvals(Arc::new(ApprovalGate::new(Arc::new(Refuse(
            asked.clone(),
            wait,
        )))))
        .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
        .build()
        .unwrap();
    World {
        _dir: dir,
        ark,
        o,
        m,
        ran,
        asked,
    }
}

impl World {
    async fn agent(&self, name: &str, spec: impl FnOnce(AgentSpec) -> AgentSpec) {
        self.o
            .spawn(spec(AgentSpec::new(name).model("m")))
            .await
            .unwrap();
    }

    /// A new conversation of `agent` in `mode` (the person chose it unless it is Auto).
    fn chat(&self, agent: &str, mode: ExecutionMode) -> String {
        let id = self.o.start_thread(agent).unwrap();
        self.set_mode(&id, mode);
        id
    }

    fn set_mode(&self, chat: &str, mode: ExecutionMode) {
        let mut s = Session::open(self.ark.engine(), chat).unwrap().unwrap();
        s.set_execution_mode(self.ark.engine(), s.execution_mode_revision(), mode)
            .unwrap();
    }

    fn ran(&self) -> Vec<String> {
        self.ran.lock().unwrap().clone()
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    fn reset(&self) {
        self.ran.lock().unwrap().clear();
        self.asked.lock().unwrap().clear();
    }
}

fn said(text: &str) -> TaskSpec {
    let mut t = TaskSpec::new(text);
    t.person = true;
    t
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

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn the_owners_sequence_in_an_auto_chat_asks_nothing() {
    let w = world(false);
    w.agent("ceo", |s| s).await;
    // The agent's own chat nobody chose a mode for (no thread named, no record yet).
    w.o.run("ceo", said("zzreplay")).await.unwrap();
    assert_eq!(w.ran(), strs(&REPLAY));
    assert_eq!(w.asked(), Vec::<String>::new());
    // A chat of its own, left in Auto.
    w.reset();
    let chat = w.chat("ceo", ExecutionMode::Auto);
    w.o.run("ceo", said("zzreplay").thread(&chat))
        .await
        .unwrap();
    assert_eq!(w.ran(), strs(&REPLAY));
    assert_eq!(w.asked(), Vec::<String>::new());
}

#[tokio::test]
async fn manual_asks_before_every_change_and_plan_offers_none() {
    let w = world(false);
    w.agent("ceo", |s| s).await;
    let chat = w.chat("ceo", ExecutionMode::Manual);
    w.o.run("ceo", said("zzreplay").thread(&chat))
        .await
        .unwrap();
    assert_eq!(w.ran(), Vec::<String>::new());
    assert_eq!(w.asked(), strs(&REPLAY));
    w.reset();
    let chat = w.chat("ceo", ExecutionMode::Plan);
    w.o.run("ceo", said("zzreplay").thread(&chat))
        .await
        .unwrap();
    assert_eq!(w.ran(), Vec::<String>::new());
    assert_eq!(w.asked(), Vec::<String>::new());
}

#[tokio::test]
async fn auto_still_asks_what_asks_every_time_and_what_only_the_person_may_run() {
    for alone in [false, true] {
        let w = world(false);
        w.agent("ceo", |s| s.allow_mutate(alone)).await;
        let chat = w.chat("ceo", ExecutionMode::Auto);
        w.o.run("ceo", said("zzask").thread(&chat)).await.unwrap();
        assert_eq!(w.ran(), strs(&["process.start"]), "alone={alone}");
        assert_eq!(
            w.asked(),
            strs(&["computer.look", "pay.send"]),
            "alone={alone}"
        );
    }
}

#[tokio::test]
async fn the_authority_line_is_the_same_in_every_mode() {
    let w = world(false);
    w.agent("ceo", |s| s).await;
    for mode in [
        ExecutionMode::Auto,
        ExecutionMode::Manual,
        ExecutionMode::Plan,
    ] {
        let chat = w.chat("ceo", mode);
        w.o.run("ceo", said("zzping").thread(&chat)).await.unwrap();
    }
    let seen = w.m.seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    for (n, text) in seen.iter().enumerate() {
        assert!(
            text.contains("Some changes ask for approval first"),
            "mode {n}"
        );
        assert!(!text.contains("Your changes ask"), "mode {n}");
        assert!(
            !text.contains("You make changes without asking"),
            "mode {n}"
        );
    }
}

#[tokio::test]
async fn only_the_persons_own_turn_follows_the_chat_the_rest_asks() {
    let w = world(false);
    w.agent("ceo", |s| s.delegates_to(["helper"])).await;
    w.agent("helper", |s| s).await;
    let chat = w.chat("ceo", ExecutionMode::Auto);
    // A turn the group driver makes for a member, and an `@Name` hand-on is one too.
    let mut group = TaskSpec::new("zzreplay").thread(&chat);
    group.group_turn = true;
    w.o.run("ceo", group).await.unwrap();
    assert_eq!(w.ran(), Vec::<String>::new(), "group turn");
    assert_eq!(w.asked(), strs(&REPLAY), "group turn");
    // The person's own turn in the same chat runs free.
    w.reset();
    w.o.run("ceo", said("zzreplay").thread(&chat))
        .await
        .unwrap();
    assert_eq!(w.ran(), strs(&REPLAY));
    // Work another agent hands on, by delegation and by `agents.start`.
    for marker in ["zzdelegate", "zzkick"] {
        w.reset();
        let chat = w.chat("ceo", ExecutionMode::Auto);
        w.o.run("ceo", said(marker).thread(&chat)).await.unwrap();
        until("helper ran", || {
            w.o.tasks()
                .iter()
                .any(|t| t.owner == "helper" && t.state.is_final())
        })
        .await;
        assert_eq!(w.ran(), Vec::<String>::new(), "{marker}");
        assert_eq!(w.asked(), strs(&REPLAY), "{marker}");
        w.o.tasks()
            .iter()
            .filter(|t| t.owner == "helper")
            .for_each(|t| assert!(t.state.is_final()));
        w.reset();
        w.o.run("helper", TaskSpec::new("zzping")).await.unwrap();
    }
}

#[tokio::test]
async fn a_scheduled_run_asks_in_an_auto_chat() {
    let w = world(false);
    w.agent("ceo", |s| s).await;
    let chat = w.chat("ceo", ExecutionMode::Auto);
    w.o.run("ceo", said("zzping").thread(&chat)).await.unwrap();
    let s =
        w.o.schedule(
            "ceo",
            TaskSpec::new("zzreplay").report_to(&chat),
            When::daily_at("02:00").in_zone("UTC"),
        )
        .unwrap();
    let id = w.o.run_schedule_now(&s.id).await.unwrap();
    w.o.wait(&id).await.unwrap();
    assert_eq!(w.ran(), Vec::<String>::new());
    assert_eq!(w.asked(), strs(&REPLAY));
}

#[tokio::test]
async fn a_thread_naming_another_agents_chat_is_refused() {
    let w = world(false);
    w.agent("ceo", |s| s).await;
    w.agent("other", |s| s).await;
    let theirs = w.chat("other", ExecutionMode::Auto);
    let refused = w.o.submit("ceo", said("zzreplay").thread(&theirs));
    assert!(refused.is_err());
    assert_eq!(w.ran(), Vec::<String>::new());
}

#[tokio::test]
async fn a_queued_task_reads_the_mode_when_it_starts() {
    let w = world(false);
    w.agent("ceo", |s| s).await;
    let chat = w.chat("ceo", ExecutionMode::Manual);
    let first = w.o.submit("ceo", said("zzblock").thread(&chat)).unwrap();
    let second = w.o.submit("ceo", said("zzreplay").thread(&chat)).unwrap();
    until("the first runs", || {
        w.o.task(&first)
            .is_some_and(|t| t.state == TaskState::Running)
    })
    .await;
    assert_eq!(w.o.task(&second).unwrap().state, TaskState::Queued);
    w.set_mode(&chat, ExecutionMode::Auto);
    w.block_notify();
    w.o.wait(&second).await.unwrap();
    assert_eq!(w.ran(), strs(&REPLAY));
    assert_eq!(w.asked(), Vec::<String>::new());
}

impl World {
    fn block_notify(&self) {
        self.m.block.notify_one();
    }
}

/// The turn hands work off and is held before it ends, so the work waits, queued, until `then`
/// has changed the chat. Only one task runs at a time. Returns once the work has ended.
async fn hand_off(w: &World, chat: &str, group: bool, then: impl FnOnce()) {
    let mut turn = said("zzhand").thread(chat);
    turn.group_turn = group;
    turn.person = !group;
    let id = w.o.submit("ceo", turn).unwrap();
    until("the work is queued", || {
        w.o.tasks()
            .iter()
            .any(|t| t.reply && t.state == TaskState::Queued)
    })
    .await;
    then();
    w.m.block.notify_one();
    w.o.wait(&id).await.unwrap();
    until("the work ends", || {
        let t = w.o.tasks();
        t.iter().filter(|t| t.reply).all(|t| t.state.is_final())
    })
    .await;
}

#[tokio::test]
async fn the_work_a_turn_hands_off_reads_the_chats_mode_when_it_starts() {
    // Auto at the hand-off and when the work starts: no question.
    let w = world(true);
    w.agent("ceo", |s| s).await;
    let chat = w.chat("ceo", ExecutionMode::Auto);
    hand_off(&w, &chat, false, || ()).await;
    assert_eq!(w.ran(), strs(&["process.start"]));
    assert_eq!(w.asked(), Vec::<String>::new());

    // Manual at the hand-off, Auto when the work starts: the person's latest choice counts.
    let w = world(true);
    w.agent("ceo", |s| s).await;
    let chat = w.chat("ceo", ExecutionMode::Manual);
    hand_off(&w, &chat, false, || w.set_mode(&chat, ExecutionMode::Auto)).await;
    assert_eq!(w.ran(), strs(&["process.start"]));
    assert_eq!(w.asked(), Vec::<String>::new());

    // Auto at the hand-off, Manual when the work starts.
    let w = world(true);
    w.agent("ceo", |s| s).await;
    let chat = w.chat("ceo", ExecutionMode::Auto);
    hand_off(&w, &chat, false, || {
        w.set_mode(&chat, ExecutionMode::Manual)
    })
    .await;
    assert_eq!(w.ran(), Vec::<String>::new());
    assert_eq!(w.asked(), strs(&["process.start"]));
}

#[tokio::test]
async fn the_work_a_group_turn_hands_off_asks() {
    let w = world(true);
    w.agent("ceo", |s| s).await;
    let chat = w.chat("ceo", ExecutionMode::Auto);
    hand_off(&w, &chat, true, || ()).await;
    assert_eq!(w.ran(), Vec::<String>::new());
    assert_eq!(w.asked(), strs(&["process.start"]));
}

#[tokio::test]
async fn a_task_made_to_look_like_the_work_of_a_turn_asks() {
    let w = world(false);
    w.agent("ceo", |s| s).await;
    let chat = w.chat("ceo", ExecutionMode::Auto);
    let turn = w.o.run("ceo", said("zzping").thread(&chat)).await.unwrap();
    // Every field a client may set: the turn it names, its chat, a reply, its own thread.
    let mut forged = TaskSpec::new("zzreplay")
        .work(&turn.id)
        .report_to(&chat)
        .thread(&chat);
    forged.reply = true;
    w.o.run("ceo", forged).await.unwrap();
    assert_eq!(w.ran(), Vec::<String>::new());
    assert_eq!(w.asked(), strs(&REPLAY));
}

#[tokio::test]
async fn the_work_of_a_turn_in_another_agents_chat_does_not_take_its_mode() {
    let w = world(false);
    w.agent("ceo", |s| s).await;
    w.agent("other", |s| s).await;
    let mine = w.chat("ceo", ExecutionMode::Auto);
    let turn = w.o.run("ceo", said("zzping").thread(&mine)).await.unwrap();
    let theirs = w.chat("other", ExecutionMode::Auto);
    let mut forged = TaskSpec::new("zzreplay").work(&turn.id).report_to(&theirs);
    forged.reply = true;
    w.o.run("ceo", forged).await.unwrap();
    assert_eq!(w.ran(), Vec::<String>::new());
    assert_eq!(w.asked(), strs(&REPLAY));
}

#[tokio::test]
async fn a_scheduled_run_of_an_agent_that_acts_alone_asks_for_what_asks_every_time() {
    let w = world(false);
    w.agent("ceo", |s| s.allow_mutate(true)).await;
    let chat = w.chat("ceo", ExecutionMode::Auto);
    w.o.run("ceo", said("zzping").thread(&chat)).await.unwrap();
    let s =
        w.o.schedule(
            "ceo",
            TaskSpec::new("zzask").report_to(&chat),
            When::daily_at("02:00").in_zone("UTC"),
        )
        .unwrap();
    let id = w.o.run_schedule_now(&s.id).await.unwrap();
    w.o.wait(&id).await.unwrap();
    assert_eq!(w.ran(), strs(&["process.start"]));
    assert_eq!(w.asked(), strs(&["computer.look", "pay.send"]));
}

#[tokio::test]
async fn an_unanswered_question_keeps_the_run_waiting_until_it_is_stopped() {
    let w = world_with(false, true);
    w.agent("ceo", |s| s.allow_mutate(true)).await;
    let chat = w.chat("ceo", ExecutionMode::Auto);
    w.o.run("ceo", said("zzping").thread(&chat)).await.unwrap();
    let s =
        w.o.schedule(
            "ceo",
            TaskSpec::new("zzask").report_to(&chat),
            When::daily_at("02:00").in_zone("UTC"),
        )
        .unwrap();
    let id = w.o.run_schedule_now(&s.id).await.unwrap();
    until("it waits for the person", || {
        w.o.task(&id).is_some_and(|t| {
            t.state == TaskState::Waiting && t.waiting_on.as_deref() == Some(WAITING_PERSON)
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let waiting = w.o.task(&id).unwrap();
    assert_eq!(waiting.state, TaskState::Waiting, "no timeout ends it");
    let question = waiting.question.expect("its question stays");
    assert_eq!(question.request.action, "computer.look");
    assert!(w.o.questions().iter().any(|t| t.id == id));
    assert_eq!(w.ran(), strs(&["process.start"]));
    w.o.cancel_and_wait(&id).await.unwrap();
    assert_eq!(w.o.task(&id).unwrap().state, TaskState::Cancelled);
}

/// ADR 0128: what asks once a turn runs in the person's own Auto turn, and asks in a scheduled
/// run (even of an agent that acts alone) and in work an agent starts. A no is never remembered,
/// so each refused call asks.
#[tokio::test]
async fn the_screen_runs_in_the_persons_auto_turn_and_asks_in_work_nobody_watches() {
    let w = world(false);
    w.agent("ceo", |s| s.allow_mutate(true).delegates_to(["helper"]))
        .await;
    w.agent("helper", |s| s.allow_mutate(true)).await;
    let chat = w.chat("ceo", ExecutionMode::Auto);
    w.o.run("ceo", said("zzscreen").thread(&chat))
        .await
        .unwrap();
    assert_eq!(w.ran(), strs(&["screen.look", "screen.click"]));
    assert_eq!(w.asked(), Vec::<String>::new());

    w.reset();
    let s =
        w.o.schedule(
            "ceo",
            TaskSpec::new("zzscreen").report_to(&chat),
            When::daily_at("02:00").in_zone("UTC"),
        )
        .unwrap();
    let id = w.o.run_schedule_now(&s.id).await.unwrap();
    w.o.wait(&id).await.unwrap();
    assert_eq!(w.ran(), Vec::<String>::new(), "scheduled");
    assert_eq!(
        w.asked(),
        strs(&["screen.look", "screen.click"]),
        "scheduled"
    );

    w.reset();
    let chat = w.chat("ceo", ExecutionMode::Auto);
    w.o.run("ceo", said("zzkickscreen").thread(&chat))
        .await
        .unwrap();
    until("helper ran", || {
        w.o.tasks()
            .iter()
            .any(|t| t.owner == "helper" && t.state.is_final())
    })
    .await;
    assert_eq!(w.ran(), Vec::<String>::new(), "started");
    assert_eq!(w.asked(), strs(&["screen.look", "screen.click"]), "started");
}
