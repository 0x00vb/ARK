//! The terminal on a runtime served elsewhere (ADR 0024). It speaks the protocol a window
//! speaks (ADR 0016): `send`, `steer`, `cancel`, `approve` and `command`, and follows the
//! runtime's events for the turn. The chat is the one open there, so a window and a terminal
//! connected to the same runtime show the same conversation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ark_interact::inspect::{self, TranscriptTurn};
use ark_interact::remote::Client;
use ark_interact::report::{Cell, Row, Table, Tone};
use ark_interact::{Block, CommandInfo, Effect, Reply, Report, Summary};
use ark_sdk::prelude::*;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::chat::{Control, Ended, Question, Turn, Update, RETRY};

/// What to do after the connection drops mid-turn.
const LOST: &str = "The turn goes on there. Connect again to follow it.";

pub(crate) struct Remote {
    client: Client,
    detail: bool,
    commands: Vec<CommandInfo>,
    /// The input of a turn the runtime runs without this terminal having sent it (a command's
    /// `Submit`, or a turn running when the terminal connected): a `turn` with that input
    /// follows it instead of sending.
    started: Option<String>,
}

impl Remote {
    /// Check the address and the token, and read what the shell needs up front.
    pub(crate) async fn connect(url: &str, token: &str, detail: bool) -> anyhow::Result<Remote> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            anyhow::bail!("`{url}` is not an address (https://…)");
        }
        let client = Client::new(url, token);
        let hello = client.hello().await?;
        Ok(Remote {
            commands: serde_json::from_value(hello["commands"].clone()).unwrap_or_default(),
            started: hello["running"].as_str().map(str::to_string),
            client,
            detail,
        })
    }

    pub(crate) fn url(&self) -> &str {
        self.client.url()
    }

    pub(crate) fn detail(&self) -> bool {
        self.detail
    }

    pub(crate) fn commands(&self) -> Vec<CommandInfo> {
        self.commands.clone()
    }

    /// The input of the turn running there when the terminal connected.
    pub(crate) fn running(&self) -> Option<&str> {
        self.started.as_deref()
    }

    /// How many items wait for the person there.
    pub(crate) async fn waiting(&self) -> usize {
        self.client
            .call("attention", json!({}))
            .await
            .ok()
            .and_then(|v| v.as_array().map(Vec::len))
            .unwrap_or(0)
    }

    pub(crate) async fn summary(&self) -> anyhow::Result<Summary> {
        Ok(serde_json::from_value(
            self.client.call("summary", json!({})).await?,
        )?)
    }

    pub(crate) async fn recap(&self) -> Option<Report> {
        let s = self.summary().await.ok()?.session;
        if s.turns == 0 {
            return None;
        }
        let last: Vec<TranscriptTurn> = serde_json::from_value(
            self.client
                .call("transcript", json!({ "session": s.id, "limit": 2 }))
                .await
                .ok()?,
        )
        .ok()?;
        Some(inspect::recap("Continuing", &s, &last))
    }

    pub(crate) async fn execute(&mut self, line: &str) -> Reply {
        let reply = match self.client.call("command", json!({ "line": line })).await {
            Ok(v) => serde_json::from_value::<Reply>(v)
                .unwrap_or_else(|e| Report::new().error(e.to_string()).into()),
            Err(e) => return Report::new().error(format!("{e:#}")).into(),
        };
        // The runtime starts the turn a command hands to the agent; follow it.
        if let Some(Effect::Submit(text)) = &reply.effect {
            self.started = Some(text.clone());
        }
        let help = matches!(line.split_whitespace().next(), Some("/help" | "/?"));
        match help {
            true => Reply {
                report: terminal_keys(reply.report),
                ..reply
            },
            false => reply,
        }
    }

    /// `ask`: see [`crate::chat::Chat::turn`]. A turn the runtime started keeps its own.
    pub(crate) fn turn(&mut self, input: &str, ask: bool) -> Turn<'_> {
        let (tx, updates) = mpsc::unbounded_channel();
        let steer = Steer {
            client: self.client.clone(),
            session: Arc::new(Mutex::new(None)),
            stopped: Arc::new(AtomicBool::new(false)),
        };
        let follow = Follow {
            client: self.client.clone(),
            input: input.to_string(),
            ask,
            steer: steer.clone(),
            tx,
        };
        let started = self.started.take().is_some_and(|s| s == input);
        Turn {
            updates,
            control: Control::Remote(steer),
            end: Box::pin(follow.run(started)),
        }
    }
}

/// Steering and stopping the turn there.
#[derive(Clone)]
pub(crate) struct Steer {
    client: Client,
    /// The turn's chat, once the runtime said which (the current one until then).
    session: Arc<Mutex<Option<String>>>,
    /// Stopped here, maybe before the turn started there: it is stopped again once it has.
    stopped: Arc<AtomicBool>,
}

impl Steer {
    pub(crate) fn steer(&self, text: &str) {
        self.send("steer", json!({ "text": text }));
    }
    pub(crate) fn cancel(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.send("cancel", json!({}));
    }
    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
    fn send(&self, method: &'static str, mut params: Value) {
        params["session"] = json!(self.session.lock().unwrap().clone());
        let c = self.client.clone();
        tokio::spawn(async move {
            let _ = c.call(method, params).await;
        });
    }
}

/// One turn followed from the runtime's events.
struct Follow {
    client: Client,
    input: String,
    ask: bool,
    steer: Steer,
    tx: mpsc::UnboundedSender<Update>,
}

impl Follow {
    /// `started`: the runtime already runs the turn; otherwise send it.
    async fn run(self, started: bool) -> Ended {
        // Listen before anything starts, so no event is missed.
        let mut events = match self.client.events().await {
            Ok(e) => e,
            Err(e) => return Ended::failed(e, RETRY),
        };
        let mut seen = 0;
        if started {
            match self.attach().await {
                Ok(Some(seq)) => seen = seq,
                Ok(None) => return self.ended_already().await,
                Err(e) => return Ended::failed(e, RETRY),
            }
        } else if self.steer.is_stopped() {
            // Stopped before it was sent: send nothing.
            return finished(&json!({ "ok": true, "cancelled": true, "answer": "", "steps": 0 }));
        } else if let Err(e) = self
            .client
            .call("send", json!({ "text": self.input, "ask": self.ask }))
            .await
        {
            return Ended::failed(e, RETRY);
        }
        while let Some(v) = events.recv().await {
            if v["seq"].as_u64().is_some_and(|s| s <= seen) {
                continue;
            }
            if v["kind"] == "resync" && !self.still_working().await {
                return self.ended_already().await;
            }
            if let Some(end) = self.event(v) {
                return end;
            }
        }
        Ended::failed(
            format!("lost the connection to ARK at {}", self.client.url()),
            LOST,
        )
    }

    /// Catch up with a turn already running: its events so far and its open questions. The
    /// sequence number they reach, or `None` when no turn runs any more.
    async fn attach(&self) -> anyhow::Result<Option<u64>> {
        let hello = self.client.hello().await?;
        if hello["running"].is_null() {
            return Ok(None);
        }
        let session = hello["summary"]["session"]["id"]
            .as_str()
            .map(str::to_string);
        *self.steer.session.lock().unwrap() = session;
        // Questions from the open list only: the replay also has ones already answered.
        for v in hello["live"].as_array().into_iter().flatten() {
            if v["kind"] != "approval" {
                self.event(v.clone());
            }
        }
        for q in hello["approvals"].as_array().into_iter().flatten() {
            self.event(json!({ "kind": "approval", "session": q["session"], "id": q["id"], "request": q["request"], "line": q["line"] }));
        }
        Ok(hello["seq"].as_u64())
    }

    fn session(&self) -> Option<String> {
        self.steer.session.lock().unwrap().clone()
    }

    /// Pass one runtime event on; the end of the turn when it is that.
    fn event(&self, v: Value) -> Option<Ended> {
        let kind = v["kind"].as_str().unwrap_or_default();
        if kind == "approval_closed" {
            let _ = self.tx.send(Update::Closed(v["id"].as_u64().unwrap_or(0)));
            return None;
        }
        // The turn's chat is the one that starts with this input after the send.
        if kind == "turn_started" && self.session().is_none() && v["input"] == json!(self.input) {
            *self.steer.session.lock().unwrap() = v["session"].as_str().map(str::to_string);
            // A stop sent while the turn was on its way may have found nothing to stop.
            if self.steer.is_stopped() {
                self.steer.cancel();
            }
        }
        let ours = self.session().is_some_and(|s| v["session"] == json!(s));
        if !ours {
            return None;
        }
        let send = |u| {
            let _ = self.tx.send(u);
        };
        match kind {
            "text" => send(Update::Event(
                HarnessEvent::TextDelta {
                    text: v["text"].as_str().unwrap_or_default().to_string(),
                },
                None,
            )),
            "event" => {
                if let Ok(ev) = serde_json::from_value(v["event"].clone()) {
                    let a = serde_json::from_value(v["activity"].clone()).ok();
                    send(Update::Event(ev, a));
                }
            }
            // An orchestrated agent's question belongs to its task, not to this chat.
            "approval" if v["request"]["agent"].is_null() => {
                if let Ok(request) = serde_json::from_value(v["request"].clone()) {
                    let line = v["line"].as_str().unwrap_or_default().to_string();
                    let id = v["id"].as_u64().unwrap_or(0);
                    send(Update::Ask(Question::remote(
                        &self.client,
                        id,
                        request,
                        line,
                    )));
                }
            }
            "turn_finished" => return Some(finished(&v)),
            _ => {}
        }
        None
    }

    /// Whether the turn's chat still runs a turn there.
    async fn still_working(&self) -> bool {
        let Ok(w) = self.client.call("working", json!({})).await else {
            return true;
        };
        let s = self.session();
        w.as_array()
            .into_iter()
            .flatten()
            .any(|x| s.is_none() || x["session"] == json!(s))
    }

    /// The turn ended before its end could be followed: show it from the stored transcript.
    async fn ended_already(&self) -> Ended {
        let mut params = json!({ "limit": 1 });
        if let Some(s) = self.session() {
            params["session"] = json!(s);
        }
        let last = self
            .client
            .call("transcript", params)
            .await
            .ok()
            .and_then(|v| serde_json::from_value::<Vec<TranscriptTurn>>(v).ok())
            .and_then(|mut v| v.pop());
        let Some(t) = last else {
            return Ended::failed("the turn ended there; /trace shows it", RETRY);
        };
        let _ = self.tx.send(Update::Event(
            HarnessEvent::Answer {
                text: t.answer.clone(),
                streamed: false,
            },
            None,
        ));
        finished(&json!({
            "ok": t.unfinished.is_none(),
            "error": t.unfinished,
            "cancelled": t.cancelled,
            "answer": t.answer,
            "footer": t.footer,
            "session": self.session(),
        }))
    }
}

/// A `turn_finished` event as the views and `--json` show it.
fn finished(v: &Value) -> Ended {
    let strings =
        |k: &str| -> Vec<String> { serde_json::from_value(v[k].clone()).unwrap_or_default() };
    let mut result = if v["ok"] == json!(true) {
        json!({ "ok": true, "answer": v["answer"], "cancelled": v["cancelled"], "steps": v["steps"] })
    } else {
        json!({ "ok": false, "error": v["error"] })
    };
    if !v["out_of_credit"].is_null() {
        result["out_of_credit"] = v["out_of_credit"].clone();
    }
    result["session"] = v["session"].clone();
    if let Some(p) = v["summary"]["project"]["name"].as_str() {
        result["project"] = json!(p);
    }
    Ended {
        error: (v["ok"] != json!(true))
            .then(|| v["error"].as_str().unwrap_or("the turn failed").to_string()),
        hint: v["out_of_credit"]["hint"]
            .as_str()
            .unwrap_or(RETRY)
            .to_string(),
        cancelled: v["cancelled"] == json!(true),
        footer: strings("footer"),
        undelivered: strings("undelivered"),
        result,
    }
}

/// `/help` from a runtime lists its window's keys; a terminal shows its own.
fn terminal_keys(mut report: Report) -> Report {
    if let Some(i) = report
        .blocks
        .iter()
        .position(|b| matches!(b, Block::Heading { text } if text == "Keys"))
    {
        report.blocks.truncate(i);
    }
    let mut t = Table::plain(2);
    for (k, v) in crate::KEYS {
        t.push(Row {
            cells: vec![Cell::new(*k, Tone::Accent), Cell::new(*v, Tone::Muted)],
            tone: Tone::Normal,
            command: None,
        });
    }
    report.heading("Keys").table(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::Turn;
    use ark_interact::{App, Command, Interaction};
    use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
    use ark_ui::access::Tokens;
    use ark_ui::{Access, ArkUi};

    struct Notes;

    #[async_trait::async_trait]
    impl Adapter for Notes {
        fn name(&self) -> &str {
            "notes"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![
                ActionDescriptor::new("notes.save", "save the note", RiskTier::Mutate),
                ActionDescriptor::new("notes.wait", "wait a moment", RiskTier::Read),
            ]
        }
        async fn run(
            &self,
            action: &str,
            _i: Value,
            _c: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            if action == "notes.wait" {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Ok(ActionOutput::json(json!({ "done": true })))
        }
    }

    /// `/save`: hands the request to the agent.
    struct SaveCmd;

    #[async_trait::async_trait]
    impl Command for SaveCmd {
        fn name(&self) -> &str {
            "save"
        }
        fn summary(&self) -> &str {
            "save the note"
        }
        async fn run(&self, _ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
            Ok(Reply::submit("save the note"))
        }
    }

    /// Saves the note (a state-changing call, so it asks), then answers "Saved.".
    fn app(dir: &std::path::Path) -> App {
        let model = MockProvider::new("m", vec![]).with_fallback(|req| {
            // "keep going": a turn that only ends when it is stopped.
            if req
                .messages
                .iter()
                .any(|m| m.content.contains("keep going"))
            {
                return Completion {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: format!("c{}", req.messages.len()),
                        name: "notes.wait".into(),
                        // Each call its own, so none is answered from an earlier one.
                        arguments: json!({ "n": req.messages.len() }),
                    }],
                    usage: Usage::default(),
                };
            }
            let done = req.messages.iter().any(|m| m.tool_call_id.is_some());
            Completion {
                content: if done { "Saved.".into() } else { String::new() },
                tool_calls: (!done)
                    .then(|| ToolCall {
                        id: "c".into(),
                        name: "notes.save".into(),
                        arguments: json!({}),
                    })
                    .into_iter()
                    .collect(),
                usage: Usage::default(),
            }
        });
        let provider: Arc<dyn ark_model::Provider> = Arc::new(model);
        let ark = Ark::builder()
            .store_root(dir)
            .tool(Arc::new(Notes))
            .profile(
                AgentProfile::builder()
                    .core_actions(["notes.save", "notes.wait"])
                    .build(),
            )
            .provider(provider)
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        App::new("Test").ark(ark).command(Arc::new(SaveCmd))
    }

    /// A runtime served with saved tokens, its address and a token for it.
    async fn serve() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::in_store(dir.path());
        let token = tokens.create("terminal").unwrap();
        let (_host, served) = ArkUi::new(app(dir.path()))
            .access(Access::Tokens(tokens))
            .serve_background("127.0.0.1:0")
            .await
            .unwrap();
        (dir, format!("http://127.0.0.1:{}", served.port), token)
    }

    /// What a turn showed, and the ids of questions closed elsewhere.
    struct Seen {
        end: Ended,
        events: Vec<HarnessEvent>,
        closed: Vec<u64>,
    }

    async fn drive(mut turn: Turn<'_>, mut ask: impl FnMut(Question)) -> Seen {
        let (mut events, mut closed) = (Vec::new(), Vec::new());
        let end = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                tokio::select! {
                    biased;
                    Some(u) = turn.updates.recv() => match u {
                        Update::Event(ev, _) => events.push(ev),
                        Update::Ask(q) => ask(q),
                        Update::Closed(id) => closed.push(id),
                    },
                    end = &mut turn.end => break end,
                }
            }
        })
        .await
        .expect("the turn ends");
        while let Ok(u) = turn.updates.try_recv() {
            match u {
                Update::Event(ev, _) => events.push(ev),
                Update::Closed(id) => closed.push(id),
                Update::Ask(_) => {}
            }
        }
        Seen {
            end,
            events,
            closed,
        }
    }

    fn saved(s: &Seen) -> bool {
        s.end.error.is_none()
            && s.end.result["answer"] == "Saved."
            && s.events.iter().any(|e| {
                matches!(e, HarnessEvent::ToolResult { action, ok: true, .. } if action == "notes.save")
            })
    }

    #[tokio::test]
    async fn a_turn_runs_there_and_its_question_is_answered_here() {
        let (_dir, url, token) = serve().await;
        let mut r = Remote::connect(&url, &token, false).await.unwrap();
        assert!(r.running().is_none());
        assert!(r.recap().await.is_none(), "a new chat has nothing to recap");
        let mut asked = Vec::new();
        let seen = drive(r.turn("save the note", true), |q| {
            asked.push(q.line.clone());
            q.answer(ApprovalDecision::AllowOnce)
        })
        .await;
        assert_eq!(asked.len(), 1);
        assert!(saved(&seen), "{:?} {:?}", seen.end.error, seen.events);
        assert!(!seen.end.footer.is_empty());
        let recap = serde_json::to_string(&r.recap().await.unwrap()).unwrap();
        assert!(
            recap.contains("Continuing") && recap.contains("› save the note"),
            "{recap}"
        );
        assert_eq!(r.summary().await.unwrap().session.turns, 1);
    }

    #[tokio::test]
    async fn a_turn_nobody_can_answer_refuses_instead_of_asking() {
        let (_dir, url, token) = serve().await;
        let mut r = Remote::connect(&url, &token, false).await.unwrap();
        let seen = drive(r.turn("save the note", false), |_| panic!("nothing asks")).await;
        assert!(seen.end.error.is_none(), "{:?}", seen.end.error);
        let refused = |e: &HarnessEvent| matches!(e, HarnessEvent::NeedsApproval { action } if action == "notes.save");
        assert!(seen.events.iter().any(refused), "{:?}", seen.events);
        assert!(!saved(&seen), "the save did not run");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stopping_here_stops_the_turn_there() {
        let (_dir, url, token) = serve().await;
        let mut r = Remote::connect(&url, &token, false).await.unwrap();
        let turn = r.turn("keep going", false);
        let ctl = turn.control.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            ctl.cancel();
        });
        let seen = drive(turn, |_| {}).await;
        assert!(
            seen.end.cancelled,
            "{} {:?}",
            seen.end.result,
            seen.events.len()
        );
    }

    #[tokio::test]
    async fn a_turn_stopped_before_it_was_sent_never_runs_there() {
        let (_dir, url, token) = serve().await;
        let mut r = Remote::connect(&url, &token, false).await.unwrap();
        let turn = r.turn("keep going", false);
        turn.control.cancel();
        let seen = drive(turn, |_| {}).await;
        assert!(seen.end.cancelled && seen.end.error.is_none());
        assert_eq!(r.summary().await.unwrap().session.turns, 0);
    }

    #[tokio::test]
    async fn a_question_answered_in_another_window_goes_away_here() {
        let (_dir, url, token) = serve().await;
        let mut r = Remote::connect(&url, &token, false).await.unwrap();
        let window = Client::new(&url, &token);
        let mut ids = Vec::new();
        let seen = drive(r.turn("save the note", true), |q| {
            ids.push(q.id);
            let w = window.clone();
            let id = q.id;
            tokio::spawn(async move {
                w.call("approve", json!({ "id": id, "decision": "allow_once" }))
                    .await
                    .unwrap();
            });
        })
        .await;
        assert!(saved(&seen));
        assert_eq!(seen.closed, ids, "the menu closes when the window answers");
    }

    #[tokio::test]
    async fn the_terminal_follows_a_turn_already_running_when_it_connects() {
        let (_dir, url, token) = serve().await;
        let window = Client::new(&url, &token);
        window
            .call("send", json!({ "text": "save the note" }))
            .await
            .unwrap();
        // The turn waits on its question in the window.
        for _ in 0..200 {
            let h = window.hello().await.unwrap();
            if !h["approvals"].as_array().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let mut r = Remote::connect(&url, &token, false).await.unwrap();
        assert_eq!(r.running(), Some("save the note"));
        let mut asked = 0;
        let seen = drive(r.turn("save the note", true), |q| {
            asked += 1;
            q.answer(ApprovalDecision::AllowOnce)
        })
        .await;
        assert_eq!(asked, 1, "the waiting question is asked once");
        assert!(saved(&seen), "{:?} {:?}", seen.end.error, seen.events);
        assert!(
            seen.events
                .iter()
                .any(|e| matches!(e, HarnessEvent::ToolCall { .. })),
            "the turn so far is shown"
        );
        assert_eq!(r.summary().await.unwrap().session.turns, 1);
    }

    #[tokio::test]
    async fn another_request_while_a_turn_runs_there_is_refused_not_merged() {
        let (_dir, url, token) = serve().await;
        let window = Client::new(&url, &token);
        window
            .call("send", json!({ "text": "save the note" }))
            .await
            .unwrap();
        let mut r = Remote::connect(&url, &token, false).await.unwrap();
        let seen = drive(r.turn("something else", false), |_| {}).await;
        let error = seen.end.error.unwrap_or_default();
        assert!(error.contains("already working"), "{error}");
        assert!(
            seen.events.is_empty(),
            "the other turn is not shown as this one"
        );
    }

    #[tokio::test]
    async fn a_command_that_hands_text_to_the_agent_is_followed_not_sent_again() {
        let (_dir, url, token) = serve().await;
        let mut r = Remote::connect(&url, &token, false).await.unwrap();
        let reply = r.execute("/save").await;
        assert_eq!(reply.effect, Some(Effect::Submit("save the note".into())));
        let seen = drive(r.turn("save the note", true), |q| {
            q.answer(ApprovalDecision::AllowOnce)
        })
        .await;
        assert!(saved(&seen), "{:?} {:?}", seen.end.error, seen.events);
        assert_eq!(r.summary().await.unwrap().session.turns, 1);
    }

    #[tokio::test]
    async fn help_lists_the_terminals_keys_and_bad_access_is_explained() {
        let (_dir, url, token) = serve().await;
        let mut r = Remote::connect(&url, &token, false).await.unwrap();
        let help = serde_json::to_string(&r.execute("/help").await.report).unwrap();
        assert!(help.contains("complete a /command") && help.contains("/save"));
        assert!(
            !help.contains("⌘K"),
            "the window's keys are not the terminal's"
        );

        let refused = Remote::connect(&url, "ark_wrong", false)
            .await
            .err()
            .unwrap();
        assert!(format!("{refused}").contains("refused the access token"));
        let bare = Remote::connect("ark.example.com", &token, false)
            .await
            .err()
            .unwrap();
        assert!(format!("{bare}").contains("not an address"));
    }
}
