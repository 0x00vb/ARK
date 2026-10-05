//! The workspace host: one [`Interaction`] behind a small request/event protocol. Transports
//! (the desktop window, the local browser server, tests) call [`Host::call`] and subscribe to
//! [`Host::subscribe`]; none of them holds agent logic.
//!
//! A turn runs in its own task and holds its conversation's interaction for its duration.
//! Requests that only read the store (projects, chats, transcripts, working state, artifacts,
//! invocations, stored traces) are served from the shared engine while it runs; the rest answer
//! from a snapshot taken after the last change, or say the agent is busy.
//!
//! Several conversations can be live at once (ADR 0017). Windows show one, the current one;
//! moving to another chat while a turn runs leaves that turn working in the background, and
//! coming back finds it. Each turn's events, and each question it asks, carry its `session`, so a
//! window shows the current chat's and lists the others as working (`working`).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ark_interact::activity::describe;
use ark_interact::ark_orchestrate::{AgentSpec, Orchestrator, TaskSpec, WorkChange};
use ark_interact::inspect::{
    ArtifactSummary, ContextSource, ContextView, KeptOut, StateView, UsageView,
};
use ark_interact::interaction::load_trace;
use ark_interact::orchestration::{event_line, Control, ControlOutcome};
use ark_interact::trace::interpret;
use ark_interact::{Effect, Interaction, Labels, PendingApproval, TurnControl};
use ark_sdk::prelude::ark_core::Focus;
use ark_sdk::prelude::ark_runtime::STATE_DIGEST_ITEMS;
use ark_sdk::prelude::*;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot};

/// Pass the orchestrator's events to every window as `{kind: "orch", event, line, tone,
/// activity}`: the line a task's timeline shows for it (none for events a timeline leaves
/// out), and for an agent's runtime event the activity the conversation would show for it
/// (none for bookkeeping such as steps and context reports). Streamed text stays out: a
/// window reads results from the graph.
async fn forward_orchestration(
    feed: Arc<Feed>,
    labels: Labels,
    mut rx: broadcast::Receiver<ark_interact::ark_orchestrate::OrchEvent>,
) {
    let names = HashMap::new();
    use ark_interact::ark_orchestrate::EventKind;
    loop {
        match rx.recv().await {
            Ok(ev) => {
                if matches!(
                    ev.kind,
                    EventKind::Agent {
                        event: HarnessEvent::TextDelta { .. },
                        ..
                    }
                ) {
                    continue;
                }
                feed.send(orch_event(&labels, &names, ev));
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                feed.send(json!({ "kind": "orch_lagged", "missed": n }));
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// `{kind: "orch", event, line, tone, activity}` for one orchestration event (see
/// [`forward_orchestration`]).
fn orch_event(
    labels: &Labels,
    names: &HashMap<String, String>,
    ev: ark_interact::ark_orchestrate::OrchEvent,
) -> Value {
    use ark_interact::ark_orchestrate::EventKind;
    let (line, tone) = event_line(&ev.kind).unzip();
    let activity = match &ev.kind {
        EventKind::Agent { event, .. } => describe(labels, names, event),
        _ => None,
    };
    json!({ "kind": "orch", "event": ev, "line": line, "tone": tone, "activity": activity })
}

/// A consumer's backend method, called from its panels as `ark.call("ext", {name, params})`.
pub type Handler = Arc<
    dyn Fn(HandlerCtx, Value) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>>
        + Send
        + Sync,
>;

/// What a consumer handler can reach.
#[derive(Clone)]
pub struct HandlerCtx {
    pub engine: Arc<Engine>,
    pub project: Project,
    emitter: Emitter,
}

impl HandlerCtx {
    /// Send an event to the consumer's scripts (`ark.on(name, fn)`).
    pub fn emit(&self, name: &str, payload: Value) {
        self.emitter.emit(name, payload)
    }
    pub fn emitter(&self) -> Emitter {
        self.emitter.clone()
    }
}

/// Events kept for a window that reconnects in the middle of a turn. Beyond this the replay
/// is cut short (the page then shows the turn from where it rejoined).
const LIVE_EVENTS: usize = 20_000;

/// The event stream every window reads. Each event gets the next sequence number, so a window
/// that reconnects can tell what it has already seen; the running turn's events are kept so a
/// window that joins late can rebuild it.
pub(crate) struct Feed {
    tx: broadcast::Sender<Value>,
    state: Mutex<FeedState>,
}

#[derive(Default)]
struct FeedState {
    seq: u64,
    /// Each running turn's events since it started, by session.
    live: HashMap<String, Vec<Value>>,
}

impl Feed {
    fn new() -> Arc<Feed> {
        let (tx, _) = broadcast::channel(8192);
        Arc::new(Feed {
            tx,
            state: Mutex::new(FeedState::default()),
        })
    }

    /// Number the event and send it. The lock keeps numbering and sending in one order.
    pub(crate) fn send(&self, mut v: Value) {
        let mut st = self.state.lock().unwrap();
        st.seq += 1;
        v["seq"] = json!(st.seq);
        let kind = v["kind"].as_str().unwrap_or_default().to_string();
        // Only a turn's own events (they carry its session) rebuild it.
        if let Some(session) = v["session"].as_str().map(str::to_string) {
            if kind == "turn_started" {
                st.live.insert(session.clone(), Vec::new());
            }
            if let Some(live) = st.live.get_mut(&session) {
                if live.len() < LIVE_EVENTS {
                    live.push(v.clone());
                }
            }
            if kind == "turn_finished" {
                st.live.remove(&session);
            }
        }
        let _ = self.tx.send(v);
    }

    /// The last sequence number sent, and `session`'s running turn's events so far.
    pub(crate) fn position(&self, session: &str) -> (u64, Vec<Value>) {
        let st = self.state.lock().unwrap();
        (st.seq, st.live.get(session).cloned().unwrap_or_default())
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }
}

/// Sends consumer events to every connected window.
#[derive(Clone)]
pub struct Emitter {
    feed: Arc<Feed>,
}

impl Emitter {
    pub fn emit(&self, name: &str, payload: Value) {
        self.feed
            .send(json!({ "kind": "ext", "name": name, "payload": payload }));
    }
}

/// What the front-end shows before any data arrives.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Welcome {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    /// (label, prompt) pairs shown as starting points.
    pub suggestions: Vec<(String, String)>,
}

/// The window's colours until the person chooses in Settings › Appearance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    /// Follow the operating system.
    #[default]
    System,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
            Theme::System => "system",
        }
    }
}

/// Presentation settings sent to the page.
#[derive(Clone, Debug, Default, Serialize)]
pub struct UiInfo {
    pub accent: Option<String>,
    pub theme: Theme,
    pub logo: Option<String>,
    pub welcome: Welcome,
    pub scripts: Vec<String>,
    pub styles: Vec<String>,
}

struct Running {
    ctl: TurnControl,
    input: String,
}

/// One live conversation: its interaction, and the turn it runs.
struct Conv {
    ix: Arc<tokio::sync::Mutex<Interaction>>,
    running: Mutex<Option<Running>>,
    /// What lists show for it, kept for while its interaction is busy.
    about: Mutex<About>,
}

#[derive(Clone, Default)]
struct About {
    session: String,
    title: String,
    project: String,
    /// Its project's orchestrator, to follow when it comes forward while busy.
    orch: Option<Orchestrator>,
    /// What `summary` answers for it.
    summary: Value,
}

impl Conv {
    fn new(ix: Interaction) -> Arc<Conv> {
        let about = About::of(&ix);
        Arc::new(Conv {
            ix: Arc::new(tokio::sync::Mutex::new(ix)),
            running: Mutex::new(None),
            about: Mutex::new(about),
        })
    }
    fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }
    fn session(&self) -> String {
        self.about.lock().unwrap().session.clone()
    }
    fn note(&self, ix: &Interaction) {
        *self.about.lock().unwrap() = About::of(ix);
    }
    /// `{session, title, project, input}` while it works.
    fn working(&self) -> Option<Value> {
        let input = self.running.lock().unwrap().as_ref()?.input.clone();
        let a = self.about.lock().unwrap().clone();
        Some(
            json!({ "session": a.session, "title": a.title, "project": a.project, "input": input }),
        )
    }
}

impl About {
    fn of(ix: &Interaction) -> About {
        let rec = ix.session().record();
        About {
            session: ix.session().id().as_str().to_string(),
            title: rec.title.clone(),
            project: ix.project().id.as_str().to_string(),
            orch: ix.orchestrator().cloned(),
            summary: serde_json::to_value(ix.summary()).unwrap_or_default(),
        }
    }
}

/// Where a change of chat lands.
enum Target {
    /// A session by id: a live one is brought forward.
    Session(String),
    /// Wherever the change leaves the conversation (a new chat, another project).
    Change,
}

/// Cached views, refreshed whenever the interaction is free.
#[derive(Default)]
struct Snapshot {
    usage: Option<UsageView>,
    context: Option<ContextView>,
    kept_out: KeptOut,
    project: Option<Project>,
}

pub struct Host {
    /// The conversation windows show.
    current: Mutex<Arc<Conv>>,
    /// Conversations still running a turn that windows no longer show. One ends when its turn
    /// does (its session is stored) unless it became current again.
    background: Mutex<Vec<Arc<Conv>>>,
    /// Opens another live conversation of the app.
    siblings: ark_interact::Siblings,
    /// Held while the windows move to another chat and while a turn ends, so the two never
    /// interleave (a chat is never opened twice, nor left behind).
    switching: tokio::sync::Mutex<()>,
    engine: Arc<Engine>,
    /// The names the conversation's skills are chosen by (`Ark::skill_names`).
    chat_skills: Vec<String>,
    /// What lists and connects capabilities while a turn holds the interaction.
    caps: ark_interact::capabilities::Parts,
    labels: Labels,
    feed: Arc<Feed>,
    approvals: Mutex<HashMap<u64, (Value, oneshot::Sender<ApprovalDecision>)>>,
    next_id: AtomicU64,
    focus: Mutex<Option<Option<Focus>>>,
    snapshot: Mutex<Snapshot>,
    handlers: HashMap<String, Handler>,
    info: UiInfo,
    commands: Value,
    /// The current project's orchestrator, and the task forwarding its events.
    orch: Mutex<Option<(Orchestrator, tokio::task::JoinHandle<()>)>>,
    /// One shell per project, for the Terminal panel.
    terminals: crate::terminal::Terminals,
    /// The Computer, when the app offers it.
    computer: Option<Arc<ark_sdk::prelude::ark_computer::ComputerService>>,
}

impl Host {
    /// Wrap an opened interaction. `approvals` is the receiving end of the channel approver
    /// the interaction was opened with. Must be called inside a tokio runtime.
    pub async fn new(
        mut ix: Interaction,
        approvals: mpsc::UnboundedReceiver<PendingApproval>,
        info: UiInfo,
        handlers: HashMap<String, Handler>,
    ) -> Arc<Host> {
        // The workspace shows things beside the conversation; offer the model `present`.
        ix.set_presentation(true);
        ix.set_keys(
            [
                ("↩", "send"),
                ("⇧↩", "new line"),
                ("↩ while working", "queue the message for after the turn"),
                ("⌘↩ while working", "send it into the running turn"),
                ("esc", "stop the agent"),
                ("⌘K", "commands"),
                ("⌘N", "new session"),
                ("⌘B", "sidebar"),
                ("⌘\\", "side panel"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );
        let host = Arc::new(Host {
            orch: Mutex::new(None),
            terminals: Default::default(),
            engine: ix.ark().shared_engine(),
            chat_skills: ix.ark().skill_names().to_vec(),
            caps: ark_interact::capabilities::Parts::of(ix.ark()),
            labels: ix.labels().clone(),
            commands: serde_json::to_value(ix.commands()).unwrap_or_default(),
            siblings: ix.siblings(),
            computer: ix.ark().computer().cloned(),
            current: Mutex::new(Conv::new(ix)),
            switching: tokio::sync::Mutex::new(()),
            background: Mutex::new(Vec::new()),
            feed: Feed::new(),
            approvals: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            focus: Mutex::new(None),
            snapshot: Mutex::new(Snapshot::default()),
            handlers,
            info,
        });
        host.refresh().await;
        let first = Arc::downgrade(&host.cur());
        tokio::spawn(forward_approvals(host.clone(), approvals, first));
        if let Some(c) = &host.computer {
            tokio::spawn(forward_computer(Arc::downgrade(&host), c.watch()));
        }
        host
    }

    /// Who uses the screen, as a window shows it: `{owner, who, session}`: `who` is the agent's
    /// name, or ARK for a chat (`session` says which); null when nobody does.
    fn computer_user(&self) -> Value {
        let Some(owner) = self.computer.as_ref().and_then(|c| c.holder()) else {
            return Value::Null;
        };
        match owner.strip_prefix("session:") {
            Some(id) => json!({ "owner": owner, "who": "ARK", "session": id }),
            None => {
                let who = owner.rsplit('/').next().unwrap_or(&owner).to_string();
                json!({ "owner": owner, "who": who, "session": null })
            }
        }
    }

    /// The conversation windows show.
    fn cur(&self) -> Arc<Conv> {
        self.current.lock().unwrap().clone()
    }

    fn is_current(&self, conv: &Arc<Conv>) -> bool {
        Arc::ptr_eq(&self.cur(), conv)
    }

    /// The live conversation of `session`: the current one or one working in the background.
    fn conv_of(&self, session: &str) -> Option<Arc<Conv>> {
        let cur = self.cur();
        if cur.session() == session {
            return Some(cur);
        }
        self.background
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.session() == session)
            .cloned()
    }

    /// The conversation a request names (`session`), else the current one.
    fn conv_for(&self, params: &Value) -> Arc<Conv> {
        params["session"]
            .as_str()
            .filter(|s| !s.is_empty())
            .and_then(|s| self.conv_of(s))
            .unwrap_or_else(|| self.cur())
    }

    /// Every conversation running a turn now, the current one first.
    fn working(&self) -> Vec<Value> {
        let mut v: Vec<Value> = self.cur().working().into_iter().collect();
        v.extend(
            self.background
                .lock()
                .unwrap()
                .iter()
                .filter_map(|c| c.working()),
        );
        v
    }

    fn emit_working(&self) {
        self.emit(json!({ "kind": "working", "working": self.working() }));
    }

    /// A new live conversation (session `session`, or a new one in `project`), with its own
    /// approval questions.
    fn open_sibling(
        self: &Arc<Self>,
        session: Option<&str>,
        project: &ProjectId,
    ) -> anyhow::Result<Arc<Conv>> {
        let (approver, rx) = ark_interact::ChannelApprover::channel();
        let ix = self.siblings.open(session, project, Arc::new(approver))?;
        let conv = Conv::new(ix);
        tokio::spawn(forward_approvals(self.clone(), rx, Arc::downgrade(&conv)));
        Ok(conv)
    }

    /// Every event from now on, each with a `seq` one higher than the last.
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.feed.subscribe()
    }

    pub fn emitter(&self) -> Emitter {
        Emitter {
            feed: self.feed.clone(),
        }
    }

    fn emit(&self, v: Value) {
        self.feed.send(v);
    }

    /// Whether the conversation windows show is running a turn.
    pub fn is_running(&self) -> bool {
        self.cur().is_running()
    }

    /// Rebuild the cached views from the current interaction (call while it is free).
    async fn refresh(&self) {
        let conv = self.cur();
        let ix = conv.ix.lock().await;
        self.refresh_with(&ix).await;
    }

    /// Call with the current conversation's interaction.
    async fn refresh_with(&self, ix: &Interaction) {
        self.cur().note(ix);
        self.follow_orchestrator(ix.orchestrator());
        let context = ix.context_view().await.ok();
        let kept_out = ix.kept_out().unwrap_or_default();
        let mut s = self.snapshot.lock().unwrap();
        s.usage = Some(ix.usage_view());
        s.context = context;
        s.kept_out = kept_out;
        s.project = Some(ix.project().clone());
    }

    /// Forward the events of the interaction's orchestrator (it changes with the project).
    fn follow_orchestrator(&self, current: Option<&Orchestrator>) {
        let mut orch = self.orch.lock().unwrap();
        let same = match (&*orch, current) {
            (Some((a, _)), Some(b)) => a.project().id == b.project().id,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if let Some((_, task)) = orch.take() {
            task.abort();
        }
        if let Some(o) = current {
            let task = tokio::spawn(forward_orchestration(
                self.feed.clone(),
                self.labels.clone(),
                o.subscribe(),
            ));
            *orch = Some((o.clone(), task));
        }
    }

    async fn control(&self, c: Control) -> anyhow::Result<ControlOutcome> {
        ark_interact::orchestration::control(&self.orchestrator()?, c).await
    }

    async fn control_done(&self, c: Control) -> anyhow::Result<Value> {
        self.control(c).await?;
        Ok(json!({}))
    }

    /// The app's saved API keys, when it keeps connections.
    fn api_keys(&self) -> Option<ark_interact::ark_sdk::prelude::ark_connect::ApiKeys> {
        self.caps.connections.as_ref().map(|c| c.api_keys().clone())
    }

    /// A board change; answers with the item as it is now.
    async fn board_control(&self, c: Control) -> anyhow::Result<Value> {
        match self.control(c).await? {
            ControlOutcome::Work(w) => Ok(serde_json::to_value(*w)?),
            _ => Ok(json!({})),
        }
    }

    /// The orchestrator, for the orchestration methods. They do not wait for the
    /// conversation: agents work on their own.
    fn orchestrator(&self) -> anyhow::Result<Orchestrator> {
        self.orch
            .lock()
            .unwrap()
            .as_ref()
            .map(|(o, _)| o.clone())
            .ok_or_else(|| anyhow::anyhow!("this app runs a single agent"))
    }

    fn project(&self) -> anyhow::Result<Project> {
        self.snapshot
            .lock()
            .unwrap()
            .project
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no project"))
    }

    fn session_id(&self) -> String {
        self.cur().session()
    }

    fn summary(&self) -> Value {
        let cur = self.cur();
        let mut v = cur.about.lock().unwrap().summary.clone();
        v["running"] = json!(cur.is_running());
        v
    }

    /// Lock the current interaction for a request that changes it; refuse while a turn runs.
    async fn idle(&self) -> anyhow::Result<tokio::sync::OwnedMutexGuard<Interaction>> {
        let conv = self.cur();
        if conv.is_running() {
            anyhow::bail!("the agent is working; wait for the turn to finish or stop it");
        }
        Ok(conv.ix.clone().lock_owned().await)
    }

    /// Handle one request. Errors become `{ "error": … }` at the transport.
    pub async fn call(self: &Arc<Self>, method: &str, params: Value) -> anyhow::Result<Value> {
        let s = |k: &str| {
            params
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let n = |k: &str| params.get(k).and_then(|v| v.as_u64());
        match method {
            "hello" => {
                let pending: Vec<Value> = self
                    .approvals
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, (req, _))| json!({ "id": id, "request": req["request"], "line": req["line"], "title": req["title"], "choices": req["choices"], "session": req["session"], "chat": req["chat"] }))
                    .collect();
                let cur = self.cur();
                let running = cur
                    .running
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|r| r.input.clone());
                // Everything up to `seq` is in this answer: stored turns through `summary`,
                // the running turn through `live`. A window ignores events at or below it.
                let (seq, live) = self.feed.position(&cur.session());
                Ok(json!({
                    "summary": self.summary(),
                    "ui": self.info,
                    "commands": self.commands,
                    "running": running,
                    "live": live,
                    "seq": seq,
                    "approvals": pending,
                    "working": self.working(),
                    "computer": self.computer_user(),
                    // How the window names a file before opening it: one list, ark-files'.
                    "file_kinds": ark_sdk::prelude::ark_files::extensions()
                        .into_iter()
                        .map(|(e, k)| (e.to_string(), json!(k.as_str())))
                        .collect::<serde_json::Map<_, _>>(),
                    // What each kind is called, for cards and tabs.
                    "kind_nouns": ark_sdk::prelude::ark_files::Kind::ALL
                        .iter()
                        .map(|k| (k.as_str().to_string(), json!(k.noun())))
                        .collect::<serde_json::Map<_, _>>(),
                    "version": env!("CARGO_PKG_VERSION"),
                }))
            }
            // The person takes the screen back from the agent using it: its next action there
            // fails and it carries on without it.
            "computer_stop" => match &self.computer {
                Some(c) => Ok(json!({ "stopped": c.take_back() })),
                None => anyhow::bail!("this app does not use the computer"),
            },
            // Every chat running a turn now (`{session, title, project, input}`).
            "working" => Ok(Value::Array(self.working())),
            "summary" => Ok(self.summary()),
            "send" => {
                let text = s("text");
                if text.trim().is_empty() {
                    anyhow::bail!("nothing to send");
                }
                let _quiet = self.switching.lock().await;
                self.start_turn(text)?;
                Ok(json!({ "started": true }))
            }
            // `session` names a chat working in the background; default the current one.
            "steer" => {
                let text = s("text");
                match self.conv_for(&params).running.lock().unwrap().as_ref() {
                    Some(r) => {
                        r.ctl.steer.push(text);
                        Ok(json!({ "queued": true }))
                    }
                    None => anyhow::bail!("no turn is running"),
                }
            }
            "cancel" => {
                if let Some(r) = self.conv_for(&params).running.lock().unwrap().as_ref() {
                    r.ctl.cancel.cancel();
                }
                Ok(json!({}))
            }
            "approve" => {
                let id = n("id").unwrap_or(0);
                let decision: ApprovalDecision =
                    serde_json::from_value(params["decision"].clone())?;
                match self.approvals.lock().unwrap().remove(&id) {
                    Some((_, reply)) => {
                        let _ = reply.send(decision);
                        self.emit(json!({ "kind": "approval_closed", "id": id }));
                        Ok(json!({}))
                    }
                    None => anyhow::bail!("that approval is no longer pending"),
                }
            }
            "command" => {
                let line = s("line");
                let mut ix = self.idle().await?;
                let reply = ix.execute(&line).await;
                self.refresh_with(&ix).await;
                drop(ix);
                if let Some(Effect::Submit(text)) = &reply.effect {
                    self.start_turn(text.clone())?;
                }
                self.emit(json!({ "kind": "summary", "summary": self.summary() }));
                Ok(serde_json::to_value(reply)?)
            }
            // Read from the store, so lists stay current while turns run. A chat running a turn
            // now says `working: true`.
            "sessions" => {
                use ark_interact::interaction::{list_sessions, project_sessions};
                let current = self.session_id();
                // One project's (by id), the current project's, or every project's.
                let project = s("project");
                let list = if !project.is_empty() {
                    project_sessions(&self.engine, &project, &current)?
                } else {
                    let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
                    let here = self.project()?.id;
                    list_sessions(&self.engine, (!all).then_some(&here), &current)?
                };
                let working: Vec<String> = self
                    .working()
                    .iter()
                    .filter_map(|w| w["session"].as_str().map(str::to_string))
                    .collect();
                let mut v = serde_json::to_value(list)?;
                for row in v.as_array_mut().into_iter().flatten() {
                    if row["id"]
                        .as_str()
                        .is_some_and(|id| working.iter().any(|w| w == id))
                    {
                        row["working"] = json!(true);
                    }
                }
                Ok(v)
            }
            "projects" => Ok(serde_json::to_value(
                ark_interact::interaction::list_projects(&self.engine, &self.project()?.id)?,
            )?),
            "models" => Ok(serde_json::to_value(
                ark_interact::ark_sdk::prelude::ark_model::catalog().await,
            )?),
            // Saved API keys (ark-connect): which providers have one, save one (checked with
            // the provider first), forget one. Without connections, keys come from the
            // environment only and these refuse.
            "api_keys" => {
                let keys = self.api_keys();
                Ok(json!({
                    "can_save": keys.is_some(),
                    "saved": keys.map(|k| k.saved()).unwrap_or_default(),
                }))
            }
            "api_key_save" => {
                let keys = self.api_keys().ok_or_else(|| {
                    anyhow::anyhow!(
                        "this app keeps no keys; set the provider's environment variable"
                    )
                })?;
                keys.save(&s("provider"), &s("key")).await?;
                Ok(json!({ "saved": keys.saved() }))
            }
            "api_key_forget" => {
                let keys = self
                    .api_keys()
                    .ok_or_else(|| anyhow::anyhow!("this app keeps no keys"))?;
                keys.forget(&s("provider"))?;
                Ok(json!({ "saved": keys.saved() }))
            }
            // What the subscription CLIs last reported about their accounts' usage limits.
            "subscription_usage" => Ok(serde_json::to_value(
                ark_interact::ark_sdk::prelude::ark_model::subscription_usage(),
            )?),
            // Moving to another chat never waits for a turn: a running one goes on in the
            // background (see `switch`).
            "new_session" => self.switch(Target::Change, |ix| ix.new_session()).await,
            "open_session" => {
                let id = s("id");
                let target = self
                    .engine
                    .store
                    .resolve_session(&id)
                    .ok()
                    .flatten()
                    .and_then(|r| r["id"].as_str().map(str::to_string))
                    .unwrap_or_else(|| id.clone());
                self.switch(Target::Session(target), move |ix| {
                    if ix.resume(&id)? {
                        Ok(())
                    } else {
                        anyhow::bail!("no session matches `{id}`")
                    }
                })
                .await
            }
            "rename_session" => {
                let (id, title) = (s("id"), s("title"));
                let current = id.is_empty() || self.session_id().starts_with(&id);
                if !current {
                    if let Some(c) = self.conv_of(&id) {
                        if c.is_running() {
                            anyhow::bail!("this chat is working; rename it when it finishes");
                        }
                    }
                }
                self.change(move |ix| {
                    let target = (!id.is_empty()).then_some(id.as_str());
                    ix.rename_session(target, &title).map(|_| ())
                })
                .await
            }
            "switch_project" => {
                let name = s("name");
                self.switch(Target::Change, move |ix| {
                    ix.switch_project(&name).map(|_| ())
                })
                .await
            }
            "open_project" => {
                let id = s("id");
                self.switch(Target::Change, move |ix| ix.open_project(&id).map(|_| ()))
                    .await
            }
            // A folder becomes a project (or opens the one it already is); a bare name is
            // a project with a scratch folder.
            "add_project" => {
                let (name, path) = (s("name"), s("path"));
                self.switch(Target::Change, move |ix| {
                    let dir =
                        (!path.trim().is_empty()).then(|| std::path::PathBuf::from(path.trim()));
                    ix.add_project(&name, dir.as_deref()).map(|_| ())
                })
                .await
            }
            "rename_project" => {
                let (id, name) = (s("id"), s("name"));
                self.change(move |ix| ix.rename_project(&id, &name).map(|_| ()))
                    .await
            }
            "project_folder" => {
                let (id, path) = (s("id"), s("path"));
                self.change(move |ix| {
                    ix.set_project_folder(&id, std::path::Path::new(path.trim()))
                        .map(|_| ())
                })
                .await
            }
            // The Terminal panel: the project's shell in its folder (see `terminal`).
            "term_open" => {
                let project = self.project()?;
                let dir = self.engine.workspace_dir(&project.id)?;
                let size = |k: &str, d: u64| n(k).unwrap_or(d).clamp(2, 1000) as u16;
                Ok(json!(self.terminals.open(
                    &self.feed,
                    project.id.as_str(),
                    dir,
                    size("cols", 80),
                    size("rows", 24)
                )?))
            }
            "term_write" => {
                self.terminals.write(&s("session"), &s("data"))?;
                Ok(Value::Null)
            }
            "term_resize" => {
                let size = |k: &str| n(k).unwrap_or(24).clamp(2, 1000) as u16;
                self.terminals
                    .resize(&s("session"), size("cols"), size("rows"))?;
                Ok(Value::Null)
            }
            "term_close" => {
                self.terminals.close(&s("session"));
                Ok(Value::Null)
            }
            // The Files panel: the project's folder (see `files`).
            "files" => {
                let dir = self.engine.workspace_dir(&self.project()?.id)?;
                let (entries, more) = crate::files::list(&dir, &s("path"))?;
                Ok(json!({ "root": dir, "entries": entries, "more": more }))
            }
            "file_read" => {
                let dir = self.engine.workspace_dir(&self.project()?.id)?;
                crate::files::read(&dir, &s("path"))
            }
            "file_write" => {
                let dir = self.engine.workspace_dir(&self.project()?.id)?;
                let base = Some(s("base")).filter(|b| !b.is_empty());
                crate::files::write(&dir, &s("path"), &s("text"), base.as_deref())
            }
            // The person's change to a spreadsheet or document: the edits the agent sends.
            "file_edit" => {
                let dir = self.engine.workspace_dir(&self.project()?.id)?;
                let base = Some(s("base")).filter(|b| !b.is_empty());
                let edits = params.get("edits").cloned().unwrap_or(Value::Null);
                crate::files::edit(&dir, &s("path"), &edits, base.as_deref())
            }
            "file_media" => {
                let dir = self.engine.workspace_dir(&self.project()?.id)?;
                crate::files::media(&dir, &s("path"))
            }
            "file_version" => {
                let dir = self.engine.workspace_dir(&self.project()?.id)?;
                crate::files::version(&dir, &s("path"))
            }
            // On the computer the runtime runs on: the window offers it only when that is the
            // person's own (`ark.isLocal()`).
            "file_open" => {
                let dir = self.engine.workspace_dir(&self.project()?.id)?;
                crate::files::open_in_app(&dir, &s("path"))?;
                Ok(Value::Null)
            }
            // The folder the project's tools work in; `path` changes it.
            "workspace" => {
                let path = s("path");
                if !path.trim().is_empty() {
                    let dir = std::path::PathBuf::from(path.trim());
                    self.change(move |ix| ix.set_workspace(&dir).map(|_| ()))
                        .await?;
                }
                // Read without waiting for a running turn.
                let project = self.project()?;
                let dir = self.engine.workspace_dir(&project.id)?;
                Ok(json!({ "path": dir, "chosen": project.workspace.is_some() }))
            }
            "model" => {
                let spec = s("spec");
                self.change(move |ix| ix.switch_model(&spec).map(|_| ()))
                    .await
            }
            // The current chat's stored turns (`session`: another chat's), while turns run too.
            "transcript" => {
                let before = n("before").map(|b| b as u32);
                let limit = n("limit").unwrap_or(30) as usize;
                let session = Some(s("session"))
                    .filter(|x| !x.is_empty())
                    .unwrap_or_else(|| self.session_id());
                Ok(serde_json::to_value(
                    ark_interact::interaction::transcript(
                        &self.engine,
                        &self.labels,
                        &session,
                        before,
                        limit,
                    )?,
                )?)
            }
            "trace" => Ok(serde_json::to_value(
                self.trace(n("index").map(|i| i as u32)).await?,
            )?),
            "call" => {
                let trace = self.trace(n("index").map(|i| i as u32)).await?;
                let k = n("n").unwrap_or(0) as usize;
                let call = trace
                    .as_ref()
                    .and_then(|t| t.call(k).cloned())
                    .ok_or_else(|| anyhow::anyhow!("no call #{k}"))?;
                let inv = match &call.invocation {
                    Some(id) => self.engine.store.get_invocation(&self.project()?.id, id)?,
                    None => None,
                };
                // How the call reads in the timeline, for the inspector.
                let entries: Vec<_> = trace
                    .map(|t| {
                        t.entries
                            .into_iter()
                            .filter(|e| e.call == Some(k))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(json!({ "call": call, "invocation": inv, "entries": entries }))
            }
            "context" => Ok(serde_json::to_value(self.context().await?)?),
            // The agent and task graph; follow it with `orch` events after its `seq`.
            "graph" => match self.orchestrator() {
                Ok(o) => Ok(serde_json::to_value(o.graph())?),
                Err(_) => Ok(Value::Null),
            },
            "task_trace" => Ok(serde_json::to_value(
                self.orchestrator()?.task_trace(&s("task"))?,
            )?),
            "task" => {
                let o = self.orchestrator()?;
                let id = s("task");
                let task = o
                    .task(&id)
                    .ok_or_else(|| anyhow::anyhow!("no task `{id}`"))?;
                // Each event with the line a timeline shows for it (from ark-interact).
                let timeline: Vec<Value> = o
                    .task_trace(&id)?
                    .into_iter()
                    .filter_map(|e| {
                        let (line, tone) = event_line(&e.kind)?;
                        Some(json!({
                            "seq": e.seq,
                            "at_ms": e.at_ms,
                            "agent": e.kind.agent(),
                            "line": line,
                            "tone": tone,
                            "event": e,
                        }))
                    })
                    .collect();
                Ok(json!({ "task": task, "timeline": timeline }))
            }
            // Stored events after `after`, worded as the live `orch` events are; a window
            // that opens late reads the recent past with it.
            "orch_events" => {
                let names = HashMap::new();
                let limit = n("limit").unwrap_or(200).min(2000) as usize;
                let events = self
                    .orchestrator()?
                    .events(n("after").unwrap_or(0), limit)?;
                Ok(Value::Array(
                    events
                        .into_iter()
                        .map(|e| orch_event(&self.labels, &names, e))
                        .collect(),
                ))
            }
            "agents" => Ok(serde_json::to_value(self.orchestrator()?.agents())?),
            // Changes to the agents go through ark-interact's `control`, as the terminal's
            // `/agent` and `/task` do; only the answer's shape is the protocol's.
            "spawn_agent" => {
                let spec: AgentSpec = serde_json::from_value(params["spec"].clone())?;
                match self.control(Control::Spawn(Box::new(spec))).await? {
                    ControlOutcome::Agent(a) => Ok(serde_json::to_value(*a)?),
                    _ => Ok(json!({})),
                }
            }
            "submit_task" => {
                let task: TaskSpec = serde_json::from_value(params["task"].clone())?;
                match self
                    .control(Control::Submit {
                        agent: s("agent"),
                        task,
                    })
                    .await?
                {
                    ControlOutcome::Submitted { task } => Ok(json!({ "task": task })),
                    _ => Ok(json!({})),
                }
            }
            "cancel_task" => self.control_done(Control::Cancel { task: s("task") }).await,
            "retry_task" => self.control_done(Control::Retry { task: s("task") }).await,
            // Stop a task an agent runs at set times (the graph's `schedules`).
            "unschedule" => {
                self.control_done(Control::Unschedule {
                    schedule: s("schedule"),
                })
                .await
            }
            // The project's board, with what the page shows of the tasks run for its items
            // and the agents an item can be given to. Follow it with `orch` events (`work`).
            "board" => {
                let o = match self.orchestrator() {
                    Ok(o) => o,
                    Err(_) => return Ok(Value::Null),
                };
                let items = o.board();
                let mut tasks = serde_json::Map::new();
                for id in items.iter().flat_map(|w| w.tasks.iter()) {
                    if let Some(t) = o.task(id) {
                        tasks.insert(
                            id.clone(),
                            json!({
                                "owner": t.owner,
                                "state": t.state,
                                "objective": t.objective,
                                "result": t.result,
                                "error": t.error,
                                "outputs": t.outputs,
                                "finished_ms": t.finished_ms,
                            }),
                        );
                    }
                }
                let agents: Vec<Value> = o
                    .agents()
                    .into_iter()
                    .filter(|a| a.state != ark_interact::ark_orchestrate::AgentState::Terminated)
                    .map(|a| json!({ "name": a.name, "role": a.spec.role, "state": a.state }))
                    .collect();
                Ok(json!({ "items": items, "tasks": tasks, "agents": agents }))
            }
            // Changes to the board go through ark-interact's `control`, as `/board` does.
            "board_add" => {
                let change: WorkChange = serde_json::from_value(params.clone())?;
                self.board_control(Control::AddWork {
                    title: s("title"),
                    change,
                })
                .await
            }
            "board_change" => {
                let change: WorkChange = serde_json::from_value(params["change"].clone())?;
                self.board_control(Control::ChangeWork {
                    item: s("item"),
                    change,
                })
                .await
            }
            "board_remove" => {
                self.control_done(Control::RemoveWork { item: s("item") })
                    .await
            }
            "board_give" => match self
                .control(Control::GiveWork {
                    item: s("item"),
                    agent: s("agent"),
                })
                .await?
            {
                ControlOutcome::Submitted { task } => Ok(json!({ "task": task })),
                _ => Ok(json!({})),
            },
            "suspend_agent" => {
                self.control_done(Control::Suspend { agent: s("agent") })
                    .await
            }
            "resume_agent" => {
                self.control_done(Control::Resume { agent: s("agent") })
                    .await
            }
            "terminate_agent" => {
                self.control_done(Control::Terminate { agent: s("agent") })
                    .await
            }
            "message_agent" => match self
                .control(Control::Message {
                    agent: s("agent"),
                    text: s("text"),
                })
                .await?
            {
                ControlOutcome::Messaged { delivered, .. } => Ok(json!({ "delivered": delivered })),
                _ => Ok(json!({})),
            },
            // The profiles the app offers, which an agent's `profile_name` chooses from.
            "profiles" => Ok(Value::Array(
                self.orchestrator()?
                    .profiles()
                    .iter()
                    .map(|p| json!({ "name": p.name, "summary": p.summary }))
                    .collect(),
            )),
            // What agents may do without asking in the open project, lasting ("Always in this
            // project"); the same as `/grants` in a terminal.
            "grants" => {
                let project = self.project()?;
                Ok(serde_json::to_value(
                    ark_sdk::prelude::ark_runtime::lasting_grants(
                        &self.engine.store,
                        project.id.as_str(),
                    ),
                )?)
            }
            "grant_remove" => {
                let project = self.project()?;
                Ok(
                    json!({ "removed": ark_sdk::prelude::ark_runtime::remove_lasting_grant(
                    &self.engine.store,
                    project.id.as_str(),
                    &s("class"),
                ) }),
                )
            }
            // What ARK can use: one list with the terminal's `/connect`. Connecting answers the
            // sign-in page for the page to open; it finishes in the background.
            "capabilities" => Ok(serde_json::to_value(
                ark_interact::capabilities::list_parts(&self.caps),
            )?),
            "capability_connect" => Ok(json!({
                "url": ark_interact::capabilities::connect(
                    self.caps.connections.as_ref(),
                    &s("id"),
                )
                .await?
            })),
            "capability_disconnect" => Ok(serde_json::to_value(
                ark_interact::capabilities::disconnect(self.caps.connections.as_ref(), &s("id"))
                    .await?,
            )?),
            // Skills of the project folder and the store; `chat` says whether the conversation
            // may read each (agents choose their own in their spec).
            "skills" => {
                let project = self.project()?;
                Ok(serde_json::to_value(ark_interact::skills::list_for(
                    &self.engine,
                    &project.id,
                    &self.chat_skills,
                ))?)
            }
            // MCP plugins: the engine's, worded and run off the async runtime by ark-interact,
            // as `/plugin` is. None waits for a running turn; a change applies from the next.
            // `plugins` is null when the app has none (it was not opened on a store root).
            "plugins" => {
                if !ark_interact::plugins::supported(&self.engine) {
                    return Ok(Value::Null);
                }
                Ok(serde_json::to_value(ark_interact::plugins::list(
                    &self.engine,
                ))?)
            }
            // `spec`: one plugin; `text`: what `/plugin add` takes (words or a JSON snippet).
            // Answers with the plugins added.
            "plugin_add" => {
                let added = match params.get("spec") {
                    Some(v) if v.is_object() => vec![
                        ark_interact::plugins::add(
                            self.engine.clone(),
                            serde_json::from_value(v.clone())?,
                        )
                        .await?,
                    ],
                    _ => ark_interact::plugins::add_text(self.engine.clone(), &s("text")).await?,
                };
                Ok(serde_json::to_value(added)?)
            }
            "plugin_remove" => {
                ark_interact::plugins::remove(self.engine.clone(), s("name")).await?;
                Ok(json!({}))
            }
            "plugin_restart" => Ok(serde_json::to_value(
                ark_interact::plugins::restart(self.engine.clone(), s("name")).await?,
            )?),
            // The actions a turn is offered, which an agent's `tools` patterns choose from.
            "tools" => {
                let mut actions =
                    ark_sdk::offered_tools(&self.engine, &self.caps.per_agent).actions();
                actions.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(Value::Array(
                    actions
                        .iter()
                        .map(|d| json!({ "name": d.name, "summary": d.summary, "risk": d.risk }))
                        .collect(),
                ))
            }
            "usage" => {
                if !self.is_running() {
                    self.refresh().await;
                }
                Ok(
                    json!({ "usage": self.snapshot.lock().unwrap().usage, "running": self.is_running() }),
                )
            }
            "state" => {
                let pid = self.project()?.id;
                let state = self
                    .engine
                    .store
                    .get_research_state(&pid)?
                    .unwrap_or_default();
                let artifacts = self.engine.store.list_artifacts(&pid)?;
                Ok(serde_json::to_value(StateView::build(
                    state,
                    STATE_DIGEST_ITEMS,
                    &artifacts,
                ))?)
            }
            "artifacts" => {
                let arts = self.engine.store.list_artifacts(&self.project()?.id)?;
                let list: Vec<ArtifactSummary> = arts.iter().map(ArtifactSummary::from).collect();
                Ok(serde_json::to_value(list)?)
            }
            "artifact" => self.artifact(&s("id"), &params),
            "invocation" => {
                let pid = self.project()?.id;
                let id = s("id");
                let id = id.trim_start_matches("inv:");
                Ok(serde_json::to_value(
                    self.engine.store.resolve_invocation(&pid, id)?,
                )?)
            }
            "run_action" => {
                let pid = self.project()?.id;
                let approved = params
                    .get("approved")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let inv = self
                    .engine
                    .run_action(&pid, &s("action"), params["input"].clone(), approved)
                    .await?;
                Ok(serde_json::to_value(inv)?)
            }
            "focus" => {
                let focus: Option<Focus> = serde_json::from_value(params["focus"].clone()).ok();
                *self.focus.lock().unwrap() = Some(focus);
                Ok(json!({}))
            }
            "ext" => {
                let name = s("name");
                let h = self
                    .handlers
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("no handler named `{name}`"))?;
                let ctx = HandlerCtx {
                    engine: self.engine.clone(),
                    project: self.project()?,
                    emitter: self.emitter(),
                };
                h(ctx, params.get("params").cloned().unwrap_or(Value::Null)).await
            }
            other => anyhow::bail!("unknown method `{other}`"),
        }
    }

    /// Apply a change to the interaction, refresh the caches, and tell every window.
    async fn change(
        self: &Arc<Self>,
        f: impl FnOnce(&mut Interaction) -> anyhow::Result<()>,
    ) -> anyhow::Result<Value> {
        let mut ix = self.idle().await?;
        f(&mut ix)?;
        self.cur().note(&ix);
        self.refresh_with(&ix).await;
        drop(ix);
        let summary = self.summary();
        self.emit(json!({ "kind": "summary", "summary": summary }));
        Ok(summary)
    }

    /// Move the windows to another chat. A chat running a turn stays live in the background and
    /// a new conversation takes its place; a chat already live (`target`, or wherever `f` lands)
    /// is brought forward as it is, so no session is ever open twice.
    async fn switch(
        self: &Arc<Self>,
        target: Target,
        f: impl FnOnce(&mut Interaction) -> anyhow::Result<()>,
    ) -> anyhow::Result<Value> {
        let _one = self.switching.lock().await;
        if let Target::Session(id) = &target {
            if self.cur().session() == *id {
                return Ok(self.summary());
            }
            if let Some(live) = self.take_background(id) {
                return self.bring_forward(live).await;
            }
        }
        let cur = self.cur();
        let before = cur.about.lock().unwrap().project.clone();
        let conv = if cur.is_running() {
            let project = self.project()?.id;
            let conv = match &target {
                // Straight into the chat asked for.
                Target::Session(id) => self.open_sibling(Some(id), &project)?,
                // Opened on the running chat's session only to move away from it at once, so
                // no empty chat is stored; it never writes there.
                Target::Change => {
                    let conv = self.open_sibling(Some(&cur.session()), &project)?;
                    f(&mut *conv.ix.lock().await)?;
                    conv
                }
            };
            conv.note(&*conv.ix.lock().await);
            if conv.session() == cur.session() {
                // The change did not move: the running chat stays where it is.
                return Ok(self.summary());
            }
            conv
        } else {
            let mut ix = cur.ix.lock().await;
            f(&mut ix)?;
            ix.follow_model();
            cur.note(&ix);
            drop(ix);
            cur.clone()
        };
        // The change landed on a chat that is live in the background: that one comes forward.
        if let Some(live) = self.take_background(&conv.session()) {
            if cur.is_running() {
                self.background.lock().unwrap().push(cur);
            }
            return self.bring_forward(live).await;
        }
        if !Arc::ptr_eq(&conv, &cur) {
            self.background.lock().unwrap().push(cur);
            *self.current.lock().unwrap() = conv.clone();
            // A conversation opened straight on a chat did not tell the app about its project.
            if matches!(target, Target::Session(_)) {
                self.announce_project(&before, &conv);
            }
        }
        self.refresh().await;
        let summary = self.summary();
        self.emit(json!({ "kind": "summary", "summary": summary }));
        self.emit_working();
        Ok(summary)
    }

    /// Tell the app (`App::on_project_change`) when the chat windows show is in another project.
    fn announce_project(&self, before: &str, now: &Arc<Conv>) {
        let project = now.about.lock().unwrap().project.clone();
        if project == before {
            return;
        }
        if let Ok(Some(p)) = self.engine.store.get_project(&ProjectId(project)) {
            self.siblings.project_changed(&p);
        }
    }

    fn take_background(&self, session: &str) -> Option<Arc<Conv>> {
        let mut bg = self.background.lock().unwrap();
        let i = bg.iter().position(|c| c.session() == session)?;
        Some(bg.remove(i))
    }

    /// Make `live` the current chat; the current one stays live only while it runs.
    async fn bring_forward(self: &Arc<Self>, live: Arc<Conv>) -> anyhow::Result<Value> {
        let old = std::mem::replace(&mut *self.current.lock().unwrap(), live.clone());
        let before = old.about.lock().unwrap().project.clone();
        if old.is_running() {
            self.background.lock().unwrap().push(old);
        }
        self.announce_project(&before, &live);
        if !live.is_running() {
            live.ix.lock().await.follow_model();
            self.refresh().await;
        } else {
            // Its interaction is busy: its project's agents now, the rest once its turn ends.
            let orch = live.about.lock().unwrap().orch.clone();
            self.follow_orchestrator(orch.as_ref());
            let mut snap = self.snapshot.lock().unwrap();
            if let Ok(Some(p)) = self
                .engine
                .store
                .get_project(&ProjectId(live.about.lock().unwrap().project.clone()))
            {
                snap.project = Some(p);
            }
        }
        let summary = self.summary();
        self.emit(json!({ "kind": "summary", "summary": summary }));
        self.emit_working();
        Ok(summary)
    }

    async fn trace(&self, index: Option<u32>) -> anyhow::Result<Option<ark_interact::Trace>> {
        if let Some(i) = index {
            return Ok(load_trace(&self.engine, &self.session_id(), i).map(|r| interpret(&r)));
        }
        let conv = self.cur();
        if let Some(r) = conv.running.lock().unwrap().as_ref() {
            let rec = r.ctl.recorder.lock().unwrap().snapshot();
            return Ok(Some(interpret(&rec)));
        }
        let t = conv.ix.lock().await.trace(None);
        Ok(t)
    }

    async fn context(&self) -> anyhow::Result<ContextView> {
        if let Some(r) = self.cur().running.lock().unwrap().as_ref() {
            // The running turn's latest request, with the store measurements from before it.
            let t = interpret(&r.ctl.recorder.lock().unwrap().snapshot());
            if let Some(report) = t.requests.last() {
                let k = self.snapshot.lock().unwrap().kept_out.clone();
                return Ok(ContextView::build(
                    report,
                    ContextSource::LastRequest,
                    None,
                    &k,
                ));
            }
        }
        if !self.is_running() {
            self.refresh().await;
        }
        self.snapshot
            .lock()
            .unwrap()
            .context
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no context yet"))
    }

    /// An artifact of the project, or of one of its orchestrated agents (each works in a
    /// child project, and what a task produced stays there).
    fn find_artifact(&self, id: &str) -> anyhow::Result<ark_sdk::prelude::Artifact> {
        let pid = self.project()?.id;
        let store = &self.engine.store;
        if let Some(a) = store.resolve_artifact(&pid, id)? {
            return Ok(a);
        }
        for p in store.list_projects()? {
            if p.parent.as_ref() == Some(&pid) {
                if let Some(a) = store.resolve_artifact(&p.id, id)? {
                    return Ok(a);
                }
            }
        }
        anyhow::bail!("artifact not found")
    }

    fn artifact(&self, id: &str, params: &Value) -> anyhow::Result<Value> {
        let art = self.find_artifact(id)?;
        let bytes = self.engine.store.read_blob(&art.id)?;
        let offset = params.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let length = params
            .get("length")
            .and_then(|v| v.as_u64())
            .unwrap_or(256 * 1024) as usize;
        let start = offset.min(bytes.len());
        let end = (start + length.min(1024 * 1024)).min(bytes.len());
        let slice = &bytes[start..end];
        let text = std::str::from_utf8(slice).ok().map(str::to_string);
        let hex = if text.is_none() || params.get("hex").and_then(|v| v.as_bool()).unwrap_or(false)
        {
            Some(slice.iter().map(|b| format!("{b:02x}")).collect::<String>())
        } else {
            None
        };
        Ok(json!({
            "artifact": ArtifactSummary::from(&art),
            "offset": start,
            "length": slice.len(),
            "total": bytes.len(),
            "text": text,
            "hex": hex,
        }))
    }

    fn start_turn(self: &Arc<Self>, input: String) -> anyhow::Result<()> {
        let conv = self.cur();
        let ctl = TurnControl::new();
        {
            let mut running = conv.running.lock().unwrap();
            if running.is_some() {
                anyhow::bail!("the agent is already working");
            }
            *running = Some(Running {
                ctl: ctl.clone(),
                input: input.clone(),
            });
        }
        let host = self.clone();
        tokio::spawn(async move { host.run_turn(conv, input, ctl).await });
        Ok(())
    }

    async fn run_turn(self: Arc<Self>, conv: Arc<Conv>, input: String, ctl: TurnControl) {
        let session = conv.session();
        self.emit(json!({ "kind": "turn_started", "input": input, "session": session }));
        self.emit_working();
        let mut ix = conv.ix.lock().await;
        if let Some(focus) = self.focus.lock().unwrap().take() {
            ix.set_focus(focus);
        }
        let feed = self.feed.clone();
        let sid = session.clone();
        let end = ix
            .run_turn(&input, &ctl, |ev, a| {
                // Text arrives in small pieces; everything else carries its description.
                let v = match ev {
                    HarnessEvent::TextDelta { text } => {
                        json!({ "kind": "text", "text": text, "session": sid })
                    }
                    _ => json!({ "kind": "event", "event": ev, "activity": a, "session": sid }),
                };
                feed.send(v);
            })
            .await;
        let (ok, cancelled, error, answer, steps) = match &end.result {
            Ok(o) => (true, o.cancelled, None, o.answer.clone(), o.steps),
            Err(e) => (false, false, Some(e.to_string()), String::new(), 0),
        };
        conv.note(&ix);
        // Not while the windows move to another chat: this decides whether it stays live.
        let one = self.switching.lock().await;
        let current = self.is_current(&conv);
        if current {
            self.refresh_with(&ix).await;
        }
        drop(ix);
        *conv.running.lock().unwrap() = None;
        // A chat in the background ends with its turn; its session is stored.
        if !current {
            self.background
                .lock()
                .unwrap()
                .retain(|c| !Arc::ptr_eq(c, &conv));
        }
        drop(one);
        // No question outlives its turn; close them before saying the turn ended. Questions
        // from orchestrated agents belong to their tasks, not to this turn.
        let open: Vec<u64> = {
            let mut approvals = self.approvals.lock().unwrap();
            let ids: Vec<u64> = approvals
                .iter()
                .filter(|(_, (shown, _))| {
                    shown["request"]["agent"].is_null() && shown["session"] == json!(session)
                })
                .map(|(id, _)| *id)
                .collect();
            for id in &ids {
                approvals.remove(id);
            }
            ids
        };
        for id in open {
            self.emit(json!({ "kind": "approval_closed", "id": id }));
        }
        self.emit(json!({
            "kind": "turn_finished",
            "session": session,
            "ok": ok,
            "cancelled": cancelled,
            "error": error,
            "answer": answer,
            "steps": steps,
            "footer": end.footer,
            "index": end.index,
            "elapsed_ms": end.elapsed_ms,
            "undelivered": end.undelivered,
            "summary": current.then(|| self.summary()),
        }));
        self.emit_working();
    }
}

/// Tell windows who uses the screen whenever that changes (`{kind: "computer", user}`).
async fn forward_computer(
    host: std::sync::Weak<Host>,
    mut rx: tokio::sync::watch::Receiver<Option<String>>,
) {
    while rx.changed().await.is_ok() {
        let Some(h) = host.upgrade() else { break };
        h.emit(json!({ "kind": "computer", "user": h.computer_user() }));
    }
}

/// Move each approval question of one conversation to the windows, and each answer back. A
/// question says which chat asks (`session`, `chat`: its title), so a window shows another
/// chat's question apart from the one it shows.
async fn forward_approvals(
    host: Arc<Host>,
    mut rx: mpsc::UnboundedReceiver<PendingApproval>,
    conv: std::sync::Weak<Conv>,
) {
    while let Some(mut pending) = rx.recv().await {
        let id = host.next_id.fetch_add(1, Ordering::SeqCst);
        let line = host.labels.approval_line(&pending.request);
        let (reply, answer) = oneshot::channel();
        let title = ark_interact::approval_title(&pending.request);
        let choices = ark_interact::approval_choices(&pending.request);
        // An orchestrated agent's question belongs to its task, not to a chat.
        let about = conv
            .upgrade()
            .filter(|_| pending.request.agent.is_none())
            .map(|c| c.about.lock().unwrap().clone());
        let (session, chat) = match &about {
            Some(a) => (json!(a.session), json!(a.title)),
            None => (Value::Null, Value::Null),
        };
        let shown = json!({ "request": pending.request, "line": line, "title": title, "choices": choices, "session": session, "chat": chat });
        host.approvals
            .lock()
            .unwrap()
            .insert(id, (shown.clone(), reply));
        host.emit(
            json!({ "kind": "approval", "id": id, "request": shown["request"], "line": line, "title": shown["title"], "choices": shown["choices"], "session": session, "chat": chat }),
        );
        let h = host.clone();
        tokio::spawn(async move {
            tokio::select! {
                d = answer => {
                    if let Ok(d) = d {
                        pending.answer(d);
                    }
                }
                _ = pending.closed() => {}
            }
            // The turn's end may have closed it already (see `run_turn`).
            if h.approvals.lock().unwrap().remove(&id).is_some() {
                h.emit(json!({ "kind": "approval_closed", "id": id }));
            }
        });
    }
}
