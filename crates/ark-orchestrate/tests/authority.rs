//! Event automations A1 (ADR 0062): the run-scoped authority, checked by the real harness before
//! every call and before the approval gate. Scripted models and fake tools; no network.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_model::{Completion, Message, Provider, ToolCall, ToolDef, Usage};
use ark_orchestrate::*;
use ark_runtime::{ApprovalDecision, ApprovalGate, ApprovalRequest, Approver};
use ark_sdk::prelude::*;
use ark_tools::events::{Direction, EventSink, ExternalEvent};
use serde_json::{json, Value};

/// Each call takes the next reply; what it was sent is kept.
struct Script {
    replies: Mutex<VecDeque<Completion>>,
    seen: Mutex<Vec<String>>,
}

impl Script {
    fn new(replies: Vec<Completion>) -> Arc<Self> {
        Arc::new(Script {
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl Provider for Script {
    async fn complete(
        &self,
        _: &str,
        messages: &[Message],
        _: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let all: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
        self.seen.lock().unwrap().push(all.join("\n"));
        let next = self.replies.lock().unwrap().pop_front();
        Ok(next.unwrap_or_else(|| answer("done")))
    }
    fn describe(&self) -> String {
        "script".into()
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
            id: "c".into(),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::measured(10, 5),
    }
}

/// Mail and a few other tools, counting what really happens.
#[derive(Default)]
struct Tools {
    sent: Mutex<Vec<Value>>,
    reads: AtomicUsize,
    uncertain: AtomicBool,
}

/// One namespace of the fake tools (`mail`, `doc`, `code`).
struct Fake(&'static str, Arc<Tools>);

#[async_trait::async_trait]
impl Adapter for Fake {
    fn name(&self) -> &str {
        self.0
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![match self.0 {
            "mail" => ActionDescriptor::new("mail.reply", "reply in the thread", RiskTier::Mutate),
            "doc" => ActionDescriptor::new("doc.read", "read a document", RiskTier::Observe),
            _ => ActionDescriptor::new("code.run", "run code", RiskTier::Observe),
        }]
    }
    async fn run(
        &self,
        action: &str,
        input: Value,
        _: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        match action {
            "mail.reply" => {
                self.1.sent.lock().unwrap().push(input.clone());
                let out = ActionOutput::json(json!({ "sent": true }));
                if self.1.uncertain.load(Ordering::SeqCst) {
                    return Ok(out.with_effect(ark_core::ExternalEffect {
                        id: "send-1".into(),
                        state: ark_core::ExternalEffectState::Unconfirmed,
                        retry_allowed: false,
                        user_message: Some("The reply may or may not have been sent.".into()),
                    }));
                }
                Ok(out)
            }
            _ => {
                self.1.reads.fetch_add(1, Ordering::SeqCst);
                Ok(ActionOutput::json(json!({ "text": "ok" })))
            }
        }
    }
}

/// Says yes to everything, "for this session" (a broader grant than once), and counts.
struct Yes(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Approver for Yes {
    async fn decide(&self, _: &ApprovalRequest) -> ApprovalDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        ApprovalDecision::AllowSession
    }
}

struct App {
    _dir: tempfile::TempDir,
    ark: Ark,
    project: Project,
    tools: Arc<Tools>,
    asked: Arc<AtomicUsize>,
}

impl App {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let tools = Arc::new(Tools::default());
        let ark = Ark::builder()
            .store_root(dir.path())
            .tool(Arc::new(Fake("mail", tools.clone())))
            .tool(Arc::new(Fake("doc", tools.clone())))
            .tool(Arc::new(Fake("code", tools.clone())))
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
            tools,
            asked: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// An automation whose agent has every tool and `allow_mutate` (the `--yes` of a terminal):
    /// only the authority decides.
    async fn automate(
        &self,
        m: &Arc<Script>,
        authority: AutomationAuthority,
    ) -> (Orchestrator, EventAutomationRecord) {
        let mm = m.clone();
        let o = Orchestrator::builder(&self.ark, &self.project)
            .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
            .approvals(Arc::new(ApprovalGate::new(Arc::new(Yes(self
                .asked
                .clone())))))
            .build()
            .unwrap();
        o.spawn(AgentSpec::new("w").model("m").allow_mutate(true))
            .await
            .unwrap();
        let a = o
            .automate(
                "w",
                TaskSpec::new("answer Lara"),
                EventPattern::new("fake", "me"),
                authority,
            )
            .unwrap();
        (o, a)
    }

    fn event(&self, key: &str) {
        EventLog::of(&self.ark.engine().store)
            .ingest(vec![ExternalEvent {
                source: "fake".into(),
                account: "me".into(),
                key: key.into(),
                kind: "message".into(),
                direction: Direction::Inbound,
                actor: Some("lara@example.com".into()),
                subject: Some(format!("th-{key}")),
                at_ms: now(),
                label: "Mail from lara@example.com".into(),
                fields: Default::default(),
                payload: Some("Please reply.".into()),
            }])
            .unwrap();
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn reply_to_sender() -> Allowed {
    Allowed::new("mail.reply")
        .input(
            "to",
            Bound::Event {
                field: "actor".into(),
            },
        )
        .input(
            "thread",
            Bound::Event {
                field: "subject".into(),
            },
        )
        .input("body", Bound::Free)
}

fn reply(to: &str, thread: &str) -> Completion {
    call(
        "mail.reply",
        json!({ "to": to, "thread": thread, "body": "Here are the totals." }),
    )
}

async fn ended(o: &Orchestrator, a: &str, n: usize) -> Vec<TaskRecord> {
    for _ in 0..1000 {
        let runs = o.automation_runs(a, 100);
        if runs.iter().filter(|t| t.state.is_final()).count() >= n {
            return runs;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("runs never ended: {:?}", o.automation_runs(a, 100));
}

fn said(m: &Script) -> String {
    m.seen.lock().unwrap().join("\n")
}

#[tokio::test]
async fn reads_code_and_unnamed_actions_are_refused_even_with_allow_mutate() {
    let app = App::new();
    let m = Script::new(vec![
        call("doc.read", json!({})),
        call("code.run", json!({ "code": "print(1)" })),
        call("search_evidence", json!({ "query": "x" })),
        call("acquire_capability", json!({ "need": "mail" })),
        answer("stopped"),
    ]);
    let (o, a) = app
        .automate(&m, AutomationAuthority::default().allow(reply_to_sender()))
        .await;
    app.event("1");
    ended(&o, &a.id, 1).await;
    assert_eq!(
        app.tools.reads.load(Ordering::SeqCst),
        0,
        "nothing outside it ran"
    );
    let seen = said(&m);
    assert!(
        seen.contains("refused: `doc.read` is not one of the actions"),
        "{seen}"
    );
    assert!(seen.contains("refused: `code.run` is not one of the actions"));
    assert!(seen.contains("refused: `search_evidence` is not available"));
    assert!(seen.contains("refused: `acquire_capability` is not available"));
}

#[tokio::test]
async fn a_reply_to_anyone_else_or_another_thread_is_refused_and_the_right_one_asks() {
    let app = App::new();
    let m = Script::new(vec![
        reply("evil@example.com", "th-1"),
        reply("lara@example.com", "th-other"),
        reply("Lara@Example.com", "th-1"),
        answer("replied"),
    ]);
    let (o, a) = app
        .automate(&m, AutomationAuthority::default().allow(reply_to_sender()))
        .await;
    app.event("1");
    ended(&o, &a.id, 1).await;
    let sent = app.tools.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["to"], "Lara@Example.com");
    assert_eq!(
        app.asked.load(Ordering::SeqCst),
        1,
        "allow_mutate did not skip the question"
    );
    assert!(said(&m).contains("must be the event's actor"));
}

#[tokio::test]
async fn an_earlier_yes_for_the_session_does_not_cover_the_next_run() {
    let app = App::new();
    let m = Script::new(vec![
        reply("lara@example.com", "th-1"),
        answer("ok"),
        reply("lara@example.com", "th-2"),
        answer("ok"),
    ]);
    let (o, a) = app
        .automate(&m, AutomationAuthority::default().allow(reply_to_sender()))
        .await;
    app.event("1");
    ended(&o, &a.id, 1).await;
    app.event("2");
    ended(&o, &a.id, 2).await;
    assert_eq!(app.asked.load(Ordering::SeqCst), 2, "asked each time");
    assert_eq!(app.tools.sent.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn unattended_replies_run_once_per_event_and_within_their_quota() {
    let app = App::new();
    let m = Script::new(vec![
        reply("lara@example.com", "th-1"),
        reply("lara@example.com", "th-1"),
        answer("ok"),
        reply("lara@example.com", "th-2"),
        answer("limited"),
    ]);
    let authority =
        AutomationAuthority::default().allow(reply_to_sender().unattended().quota(1, 86_400_000));
    let (o, a) = app.automate(&m, authority).await;
    app.event("1");
    ended(&o, &a.id, 1).await;
    assert_eq!(app.asked.load(Ordering::SeqCst), 0, "unattended");
    assert!(said(&m).contains("may run 1 time per event"));
    app.event("2");
    ended(&o, &a.id, 2).await;
    assert_eq!(app.tools.sent.lock().unwrap().len(), 1, "the quota held");
    assert!(said(&m).contains("rate limited: `mail.reply` may run 1 times per day"));
    let limited = o.automation(&a.id).unwrap().limited.unwrap();
    assert_eq!(limited.action, "mail.reply");
    assert!(limited.until_ms > now());
}

#[tokio::test]
async fn an_uncertain_send_stops_further_changes_and_retries() {
    let app = App::new();
    app.tools.uncertain.store(true, Ordering::SeqCst);
    let m = Script::new(vec![
        reply("lara@example.com", "th-1"),
        call(
            "mail.reply",
            json!({ "to": "lara@example.com", "thread": "th-1", "body": "Sending again, in other words." }),
        ),
        answer("I sent the reply."),
    ]);
    let authority = AutomationAuthority::default().allow(Allowed {
        per_run: 5,
        ..reply_to_sender().unattended()
    });
    let (o, a) = app.automate(&m, authority).await;
    app.event("1");
    let runs = ended(&o, &a.id, 1).await;
    let run = &runs[0];
    assert!(run.effect_uncertain, "{run:?}");
    assert_eq!(run.max_attempts, run.attempts, "not tried again");
    assert_eq!(app.tools.sent.lock().unwrap().len(), 1, "no second send");
    assert!(said(&m).contains("may or may not have happened"));
    assert_eq!(
        run.result.as_deref(),
        Some("The reply may or may not have been sent.")
    );
}

#[tokio::test]
async fn each_event_run_works_in_a_project_of_its_own() {
    let app = App::new();
    let m = Script::new(vec![answer("one"), answer("two")]);
    let (o, a) = app.automate(&m, AutomationAuthority::default()).await;
    app.event("1");
    ended(&o, &a.id, 1).await;
    app.event("2");
    let runs = ended(&o, &a.id, 2).await;
    let projects: Vec<String> = app
        .ark
        .engine()
        .store
        .list_projects()
        .unwrap()
        .into_iter()
        .filter(|p| runs.iter().any(|t| p.id.as_str().ends_with(&t.id)))
        .map(|p| p.id.as_str().to_string())
        .collect();
    assert_eq!(projects.len(), 2, "{projects:?}");
    assert_ne!(projects[0], projects[1]);
}

/// The person's chat on a scripted model, with the chat tools and an account offered.
async fn chat_turn(
    app: &App,
    m: &Arc<Script>,
    approvals: Option<Arc<ApprovalGate>>,
    allow_mutate: bool,
) -> (Ark, Orchestrator) {
    let ark = Ark::builder()
        .store_root(app.ark.settings().store_root.clone())
        .provider(m.clone() as Arc<dyn Provider>)
        .allow_mutate(allow_mutate)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let o = Orchestrator::builder(&ark, &app.project).build().unwrap();
    o.event_log().offer(Offered {
        source: "fake".into(),
        account: "me".into(),
        label: "Mail (me)".into(),
        actions: vec!["mail.reply".into()],
    });
    let mut s = ark.start_session(&app.project.id).unwrap();
    let id = s.id().as_str().to_string();
    let opts = TurnOptions {
        tools: vec![o.chat_tools_for(&id)],
        approvals,
        ..Default::default()
    };
    ark.ask_session(
        &mut s,
        "When Lara emails, reply in the thread.",
        opts,
        |_| {},
    )
    .await
    .unwrap();
    (ark, o)
}

fn automate_call() -> Completion {
    call(
        AUTOMATE,
        json!({
            "source": "fake", "account": "me", "from": "lara@example.com",
            "objective": "reply in the same thread using budget.csv",
            "can_read": ["budget.csv"],
            "can_do": [{ "action": "mail.reply", "inputs": { "to": "event.actor", "thread": "event.subject", "body": "free" }, "per_day": 10 }]
        }),
    )
}

#[tokio::test]
async fn the_chat_makes_an_automation_in_words_and_it_is_asked_each_time() {
    let app = App::new();
    let m = Script::new(vec![automate_call(), answer("Set up.")]);
    let asked = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(ApprovalGate::new(Arc::new(Yes(asked.clone()))));
    let (_ark, o) = chat_turn(&app, &m, Some(gate), false).await;
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    let all = o.automations();
    assert_eq!(all.len(), 1, "{}", said(&m));
    let a = &all[0];
    assert_eq!(a.pattern.actor.as_deref(), Some("lara@example.com"));
    assert_eq!(a.authority.paths, vec!["budget.csv".to_string()]);
    let reply = a.authority.allowed("mail.reply").unwrap();
    assert_eq!(reply.quota.unwrap().count, 10);
    assert_eq!(reply.mode, Mode::Ask);
    assert!(a.agent.starts_with("reply-the-same"), "{}", a.agent);
    let agent = o.agent(&a.agent).unwrap();
    assert_eq!(agent.spec.tools, vec!["mail.reply".to_string()]);
    assert!(agent.spec.delegates_to.is_empty());
}

#[tokio::test]
async fn with_nobody_to_ask_an_automation_is_not_made_even_with_allow_mutate() {
    let app = App::new();
    let m = Script::new(vec![automate_call(), answer("Could not.")]);
    let (_ark, o) = chat_turn(&app, &m, None, true).await;
    assert!(o.automations().is_empty());
    assert!(said(&m).contains("needs human approval"), "{}", said(&m));
}

#[tokio::test]
async fn no_account_offered_means_no_automation_tools() {
    let app = App::new();
    let o = Orchestrator::builder(&app.ark, &app.project)
        .build()
        .unwrap();
    o.event_log().withdraw("fake", "me");
    let names: Vec<String> = o
        .chat_tools()
        .adapter
        .actions()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert!(!names.iter().any(|n| n == AUTOMATE), "{names:?}");
}
