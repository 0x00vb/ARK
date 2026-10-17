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

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::resource::{
    ResourceAuthority, ResourceKind, ResourceLocation, ResourceOperation, ResourceOperationKind,
    ResourceRef, ResourceSubscribers,
};
use ark_interact::activity::describe;
use ark_interact::ark_orchestrate::{AgentSpec, Orchestrator, TaskSpec};
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
use base64::Engine as _;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc, oneshot};

mod copying;
mod held;

const DICTATION_CHUNK: usize = 256 * 1024;
const DICTATION_MAX: usize = 8 * 60 * 16_000 * 2;

struct DictationUpload {
    owner: String,
    file: tempfile::NamedTempFile,
    next: u64,
    bytes: usize,
    chunks: HashMap<u64, (usize, [u8; 32])>,
    touched: std::time::Instant,
}

/// The connected WhatsApp Business numbers as the page lists them: the name, the account id to
/// disconnect it by, and the path Meta sends webhooks to.
fn whatsapp_numbers(
    c: Option<&ark_interact::ark_sdk::prelude::ark_connect::Connections>,
) -> Vec<Value> {
    c.map(|c| c.whatsapp_numbers())
        .unwrap_or_default()
        .into_iter()
        .map(|(a, h)| json!({ "id": a.id, "name": a.name, "path": h.path }))
        .collect()
}

/// `text` with each of `secrets` replaced, so an error from a provider never echoes one back.
fn without(text: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .filter(|s| !s.is_empty())
        .fold(text.to_string(), |t, s| t.replace(s.as_str(), "..."))
}

fn begin_dictation_upload(
    uploads: &mut HashMap<String, DictationUpload>,
    owner: String,
) -> anyhow::Result<String> {
    if uploads.values().any(|upload| upload.owner == owner) {
        anyhow::bail!("this client already has a dictation upload");
    }
    let id = uuid::Uuid::new_v4().to_string();
    uploads.insert(
        id.clone(),
        DictationUpload {
            owner,
            file: tempfile::NamedTempFile::new()?,
            next: 0,
            bytes: 0,
            chunks: HashMap::new(),
            touched: std::time::Instant::now(),
        },
    );
    Ok(id)
}

fn write_dictation_chunk(
    upload: &mut DictationUpload,
    owner: &str,
    index: u64,
    data: &[u8],
) -> anyhow::Result<u64> {
    if upload.owner != owner {
        anyhow::bail!("dictation belongs to another client");
    }
    if data.is_empty() || data.len() > DICTATION_CHUNK {
        anyhow::bail!("dictation chunk is empty or too large");
    }
    let digest: [u8; 32] = Sha256::digest(data).into();
    if index < upload.next {
        return match upload.chunks.get(&index) {
            Some((len, hash)) if *len == data.len() && *hash == digest => Ok(upload.next),
            _ => anyhow::bail!("dictation chunk conflicts with its earlier copy"),
        };
    }
    if index != upload.next {
        anyhow::bail!("dictation chunks must arrive in order");
    }
    if upload.bytes + data.len() > DICTATION_MAX {
        anyhow::bail!("dictation is longer than eight minutes");
    }
    upload.file.write_all(data)?;
    upload.bytes += data.len();
    upload.chunks.insert(index, (data.len(), digest));
    upload.next += 1;
    upload.touched = std::time::Instant::now();
    Ok(upload.next)
}

/// Pass the orchestrator's events to every window as `{kind: "orch", event, line, tone,
/// activity}`: the line a task's timeline shows for it (none for events a timeline leaves
/// out), and for an agent's runtime event the activity the conversation would show for it
/// (none for bookkeeping such as steps and context reports). An agent's streamed text goes as
/// `{kind: "orch_text", project, task, agent, text}`, live only: it is not numbered or kept,
/// and a window that misses it reads the result from the graph.
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
                if let EventKind::Agent {
                    task,
                    agent,
                    event: HarnessEvent::TextDelta { text },
                } = &ev.kind
                {
                    feed.send_live(json!({
                        "kind": "orch_text", "project": ev.project, "task": task,
                        "agent": agent, "text": text,
                    }));
                    continue;
                }
                // A result posted into a chat: shown there live, like its turns.
                if let EventKind::Posted { session, post, .. } = &ev.kind {
                    feed.send(json!({ "kind": "post", "session": session, "post": post }));
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

/// Tell every window when what waits for the person may have changed, in any project
/// (`{kind: "attention"}`; a window reads `attention` again). A result posted into the chat a
/// window shows is seen only when that window says so (`attention_seen`): a host has a current
/// chat also when no window is open.
async fn forward_attention(
    host: std::sync::Weak<Host>,
    mut rx: broadcast::Receiver<ark_interact::ark_orchestrate::OrchEvent>,
) {
    use ark_interact::ark_orchestrate::EventKind;
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let Some(host) = host.upgrade() else { return };
                host.emit(json!({ "kind": "attention" }));
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let Some(host) = host.upgrade() else { return };
        // Streamed text goes live only, as `orch_text` (`forward_orchestration`): as an `orch`
        // event it would be numbered and kept, and each chunk would read as a change.
        if matches!(
            ev.kind,
            EventKind::Agent {
                event: HarnessEvent::TextDelta { .. },
                ..
            }
        ) {
            continue;
        }
        for reference in
            host.resource_subscribers
                .aliases(ResourceKind::Project, &ev.project, "orch")
        {
            let mut out = orch_event(&host.labels, &HashMap::new(), ev.clone());
            out["session"] = json!(reference.location.session);
            out["ref"] = json!(reference);
            host.emit(out);
        }
        match &ev.kind {
            // A retry takes a failed task out of the list (`TaskQueued`).
            EventKind::Asked { .. }
            | EventKind::TaskFinished { .. }
            | EventKind::TaskQueued { .. } => {}
            // Answered from any client (the terminal's `/waiting` too): its card goes.
            EventKind::Answered { question, .. } => {
                host.emit(json!({ "kind": "approval_closed", "id": question }));
            }
            EventKind::Posted { .. } => {}
            _ => continue,
        }
        host.notify_soon();
        host.emit(json!({ "kind": "attention" }));
    }
}

/// Tell every window when a linked runtime's list or state changed (`{kind: "attention",
/// linked: true}`).
async fn forward_links(host: std::sync::Weak<Host>, mut rx: broadcast::Receiver<()>) {
    loop {
        match rx.recv().await {
            Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                let Some(host) = host.upgrade() else { return };
                // `linked`: a runtime that follows this one does not read it again for this.
                host.emit(json!({ "kind": "attention", "linked": true }));
                for chat in ark_interact::held::list(&host.engine, None) {
                    host.emit(json!({
                        "kind": "resource_connection",
                        "location": {
                            "runtime": chat.runtime,
                            "project": chat.remote_project,
                            "session": chat.session,
                        },
                        "online": host.links.state(&chat.place) == Some(ark_interact::links::State::Ok),
                        "place": chat.place,
                    }));
                }
                let routes: Vec<ProjectResourceRoute> = {
                    let mut kept = host.project_resource_routes.lock().unwrap();
                    kept.retain(|_, route| {
                        let Some(place) = route.place.as_deref() else {
                            return true;
                        };
                        !matches!(
                            host.links.state(place),
                            None | Some(ark_interact::links::State::Refused)
                        )
                    });
                    kept.values()
                        .filter(|route| route.place.is_some())
                        .cloned()
                        .collect()
                };
                for route in routes {
                    let place = route.place.as_deref().unwrap_or("");
                    host.emit(json!({
                        "kind": "resource_connection",
                        "location": {
                            "runtime": route.runtime,
                            "project": route.local_project,
                            "session": "",
                        },
                        "online": host.links.state(place) == Some(ark_interact::links::State::Ok),
                        "place": place,
                    }));
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Project Terminal routes are capabilities for one live PTY. A natural exit invalidates every
/// alias, including aliases opened by other windows or runtimes.
async fn forward_project_terminal_exits(
    host: std::sync::Weak<Host>,
    mut rx: broadcast::Receiver<Value>,
) {
    loop {
        let event = match rx.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if event["kind"] != "term" || event["exit"] != true {
            continue;
        }
        let Ok(reference) = serde_json::from_value::<ResourceRef>(event["ref"].clone()) else {
            continue;
        };
        if !matches!(&reference.authority, ResourceAuthority::Project { .. }) {
            continue;
        }
        let Some(host) = host.upgrade() else { return };
        host.end_project_terminal_routes(
            &reference.location.runtime,
            &reference.location.project,
            &reference.id,
        );
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

/// A consumer's project backend method. It is reachable only through a validated resource
/// reference and receives that reference's explicit project.
pub type ProjectHandler = Arc<
    dyn Fn(ProjectHandlerCtx, Value) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>>
        + Send
        + Sync,
>;

/// A consumer's window-local backend method. It can never receive or infer a project from a
/// remote chat.
pub type WindowHandler = Arc<
    dyn Fn(WindowHandlerCtx, Value) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>>
        + Send
        + Sync,
>;

/// A handler has one explicit routing class. There is no default.
#[derive(Clone)]
pub enum Handler {
    Project(ProjectHandler),
    Window(WindowHandler),
}

/// What a consumer handler can reach.
#[derive(Clone)]
pub struct ProjectHandlerCtx {
    pub engine: Arc<Engine>,
    pub project: Project,
    emitter: Emitter,
    links: Arc<ark_interact::links::Links>,
}

impl ProjectHandlerCtx {
    /// The app's links to other runtimes, to change when the person links or forgets one.
    pub fn links(&self) -> Arc<ark_interact::links::Links> {
        self.links.clone()
    }
    /// Send an event to the consumer's scripts (`ark.on(name, fn)`).
    pub fn emit(&self, name: &str, payload: Value) {
        self.emitter.emit(name, payload)
    }
    pub fn emitter(&self) -> Emitter {
        self.emitter.clone()
    }
}

/// What a window-local consumer handler can reach. It deliberately has no project.
#[derive(Clone)]
pub struct WindowHandlerCtx {
    pub engine: Arc<Engine>,
    emitter: Emitter,
    links: Arc<ark_interact::links::Links>,
}

impl WindowHandlerCtx {
    pub fn links(&self) -> Arc<ark_interact::links::Links> {
        self.links.clone()
    }
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
/// The most events `task` returns of one task's work (its `feed`).
const TASK_FEED: usize = 2_000;

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
    recent: VecDeque<Value>,
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
        st.recent.push_back(v.clone());
        while st.recent.len() > 8_192 {
            st.recent.pop_front();
        }
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

    /// Send an event that is live only (streamed text): not numbered, not kept for a window
    /// that reconnects or joins late, so it cannot push kept events out of `recent`.
    pub(crate) fn send_live(&self, v: Value) {
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

    /// Events after `after`, or `None` when the retained range has a gap.
    pub(crate) fn replay_after(&self, after: u64) -> Option<Vec<Value>> {
        let st = self.state.lock().unwrap();
        if after > st.seq {
            return None;
        }
        let oldest = st.recent.front().and_then(|event| event["seq"].as_u64());
        if oldest.is_some_and(|oldest| after.saturating_add(1) < oldest) {
            return None;
        }
        Some(
            st.recent
                .iter()
                .filter(|event| event["seq"].as_u64().is_some_and(|seq| seq > after))
                .cloned()
                .collect(),
        )
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
    /// What this chat's next turn sees in the side pane. It never leaks to another chat.
    focus: Mutex<Option<Option<Focus>>>,
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
            focus: Mutex::new(None),
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

/// An ephemeral capability for one project Terminal (ADR 0060).
#[derive(Clone, Debug)]
struct ProjectResourceRoute {
    source_project: String,
    local_project: String,
    runtime: String,
    place: Option<String>,
    kind: ResourceKind,
    terminal: String,
    origin: Option<String>,
    /// Paired-device digest for an external-client route. Window routes have no owner.
    owner: Option<String>,
    generation: String,
    next_write: u64,
}

pub struct Host {
    /// The conversation windows show.
    current: Mutex<Arc<Conv>>,
    /// Conversations still running a turn that windows no longer show. One ends when its turn
    /// does (its session is stored) unless it became current again.
    background: Mutex<Vec<Arc<Conv>>>,
    /// Terminals another runtime opened here for a program's install or sign-in (`chat_setup`):
    /// the only terminals it may type into.
    setup_terms: Mutex<HashMap<String, String>>,
    /// One live setup terminal per worker spec. A reload reconnects to it instead of starting
    /// another install or sign-in process.
    worker_setup_terms: Mutex<HashMap<String, String>>,
    /// Chats currently allowed to operate each worker setup terminal. Several windows may
    /// reconnect to the same live process.
    worker_setup_owners: Mutex<HashMap<String, std::collections::HashSet<String>>>,
    /// Chats another runtime holds here and reaches by name (`chat_*`, ADR 0052), by session.
    /// None of them moves the windows; a window that opens one takes it from here.
    addressed: Mutex<HashMap<String, Arc<Conv>>>,
    /// Opens another live conversation of the app.
    siblings: ark_interact::Siblings,
    /// Held while the windows move to another chat and while a turn ends, so the two never
    /// interleave (a chat is never opened twice, nor left behind).
    switching: tokio::sync::Mutex<()>,
    /// External-client mutations are accepted in strict device order.
    client_mutations: tokio::sync::Mutex<()>,
    engine: Arc<Engine>,
    /// The names the conversation's skills are chosen by (`Ark::skill_names`).
    chat_skills: Vec<String>,
    /// What lists and connects capabilities while a turn holds the interaction.
    caps: ark_interact::capabilities::Parts,
    labels: Labels,
    feed: Arc<Feed>,
    approvals: Mutex<HashMap<u64, (Value, oneshot::Sender<ApprovalDecision>)>>,
    next_id: AtomicU64,
    snapshot: Mutex<Snapshot>,
    handlers: HashMap<String, Handler>,
    transcription: Option<Arc<dyn crate::transcription::TranscriptionProvider>>,
    dictations: Mutex<HashMap<String, DictationUpload>>,
    info: UiInfo,
    commands: Value,
    /// The current project's orchestrator, and the task forwarding its events.
    orch: Mutex<Option<(Orchestrator, tokio::task::JoinHandle<()>)>>,
    /// Every project's orchestrator: questions kept by tasks in any of them (ADR 0036).
    orchs: ark_interact::OrchestratorSet,
    /// One shell per project, for the Terminal panel.
    terminals: crate::terminal::Terminals,
    /// Session-bound pane aliases for shared terminal and process resources.
    resource_subscribers: Arc<ResourceSubscribers>,
    file_snapshots: Arc<crate::file_snapshots::Snapshots>,
    /// Server-issued project resource capabilities. They intentionally vanish on restart.
    project_resource_routes: Mutex<HashMap<String, ProjectResourceRoute>>,
    /// One reconciliation at a time per project replica. Chats share replicas, so epoch checks,
    /// filesystem apply and manifest advancement are one critical section.
    copy_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// The Computer, when the app offers it.
    computer: Option<Arc<ark_sdk::prelude::ark_computer::ComputerService>>,
    /// Project processes, when the app offers them (ADR 0041).
    processes: Option<ark_sdk::prelude::ark_proc::Processes>,
    /// Chromium on this runtime, for a person's chat-owned rendered Browser.
    browser: Option<Arc<ark_sdk::prelude::ark_browser::BrowserService>>,
    /// Agent programs offered by this runtime. Catalog and setup never cross runtime bounds.
    workers: Option<Arc<dyn ark_sdk::prelude::ark_runtime::worker::Workers>>,
    /// Notifications to the person's devices (ADR 0047).
    pub(crate) notifier: Arc<crate::notify::Notifier>,
    /// The app's links to other runtimes, shared with its conversations.
    links: Arc<ark_interact::links::Links>,
    /// The chat windows show when it is held on another computer (its session); `None` when it
    /// is `current` (ADR 0052).
    held: Mutex<Option<String>>,
    /// The person's computers over SSH, when the app offers them (ADR 0052).
    computers: std::sync::OnceLock<Arc<crate::computers::Computers>>,
    /// What this runtime knows of held chats while they run.
    held_live: Mutex<HashMap<String, held::HeldLive>>,
    /// One follower of events per place that holds chats here, and whether it is following.
    #[allow(clippy::type_complexity)]
    relays: Mutex<
        HashMap<
            String,
            (
                tokio::task::JoinHandle<()>,
                tokio::sync::watch::Receiver<bool>,
            ),
        >,
    >,
}

#[derive(Debug)]
pub(crate) enum ClientMutationError {
    InvalidCounter,
    CounterConflict,
    CounterGap,
    AlreadyApplied,
    Unsupported,
    Store(anyhow::Error),
}

impl From<anyhow::Error> for ClientMutationError {
    fn from(error: anyhow::Error) -> Self {
        Self::Store(error)
    }
}

impl Host {
    fn dictation_owner(params: &Value) -> anyhow::Result<String> {
        let owner = params
            .get("_dictation_owner")
            .and_then(Value::as_str)
            .unwrap_or("");
        if owner.is_empty() {
            anyhow::bail!("dictation requires an authenticated client");
        }
        Ok(owner.to_string())
    }

    fn sweep_dictations(&self) {
        let limit = std::time::Duration::from_secs(10 * 60);
        self.dictations
            .lock()
            .unwrap()
            .retain(|_, upload| upload.touched.elapsed() < limit);
    }

    fn dictation_begin(&self, params: &Value) -> anyhow::Result<Value> {
        self.sweep_dictations();
        let owner = Self::dictation_owner(params)?;
        let Some(provider) = self.transcription.clone() else {
            anyhow::bail!("Dictation is not included in this app.");
        };
        let mut uploads = self.dictations.lock().unwrap();
        let id = begin_dictation_upload(&mut uploads, owner)?;
        drop(uploads);
        tokio::spawn(async move {
            let _ = provider.prepare().await;
        });
        Ok(
            json!({ "id": id, "runtime": ark_interact::held::runtime_id(&self.engine), "max_bytes": DICTATION_MAX, "chunk_bytes": DICTATION_CHUNK }),
        )
    }

    fn dictation_chunk(&self, params: &Value) -> anyhow::Result<Value> {
        let owner = Self::dictation_owner(params)?;
        let id = params.get("id").and_then(Value::as_str).unwrap_or("");
        let index = params
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let encoded = params.get("data").and_then(Value::as_str).unwrap_or("");
        let data = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        let mut uploads = self.dictations.lock().unwrap();
        let upload = uploads
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("dictation expired"))?;
        let next = write_dictation_chunk(upload, &owner, index, &data)?;
        Ok(json!({ "next": next }))
    }

    fn dictation_cancel(&self, params: &Value) -> anyhow::Result<Value> {
        let owner = Self::dictation_owner(params)?;
        let id = params.get("id").and_then(Value::as_str).unwrap_or("");
        let mut uploads = self.dictations.lock().unwrap();
        if let Some(upload) = uploads.get(id) {
            if upload.owner != owner {
                anyhow::bail!("dictation belongs to another client");
            }
        }
        Ok(json!({ "cancelled": uploads.remove(id).is_some() }))
    }

    async fn dictation_finish(&self, params: &Value) -> anyhow::Result<Value> {
        let owner = Self::dictation_owner(params)?;
        let id = params.get("id").and_then(Value::as_str).unwrap_or("");
        let language = params
            .get("language")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut upload = self
            .dictations
            .lock()
            .unwrap()
            .remove(id)
            .ok_or_else(|| anyhow::anyhow!("dictation expired"))?;
        if upload.owner != owner {
            self.dictations
                .lock()
                .unwrap()
                .insert(id.to_string(), upload);
            anyhow::bail!("dictation belongs to another client");
        }
        if upload.bytes < 3_200 || upload.bytes % 2 != 0 {
            anyhow::bail!("the recording is empty or malformed");
        }
        upload.file.flush()?;
        upload.file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::with_capacity(upload.bytes);
        upload.file.read_to_end(&mut bytes)?;
        let pcm = bytes
            .chunks_exact(2)
            .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
            .collect();
        let provider = self
            .transcription
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Dictation is not included in this app."))?;
        provider.prepare().await?;
        let text = provider.transcribe(pcm, language).await?;
        Ok(json!({ "text": text.trim() }))
    }

    /// The process is about to exit (SIGTERM, a deploy, the app quitting). Every run stops at its
    /// next safe point (`Engine::stop_soon`): a change under way finishes, nothing new starts.
    /// An agent's task is saved to continue in the next process; a chat's turn ends as
    /// cancelled. Waits up to `grace`; false when some run did not stop in time (its change, if
    /// any, then shows as cut short after the restart).
    pub async fn stop(&self, grace: std::time::Duration) -> bool {
        self.engine.stop_soon();
        let settled = async {
            self.engine.runs_ended().await;
            // A run that stopped is saved as waiting for the next process just after.
            while self.orchs.list().iter().any(|o| {
                o.tasks()
                    .iter()
                    .any(|t| t.state == ark_interact::ark_orchestrate::TaskState::Running)
            }) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        };
        let settled = tokio::time::timeout(grace, settled).await.is_ok();
        // Nothing an agent started outlives the runtime (ADR 0041).
        if let Some(p) = &self.processes {
            p.stop_all().await;
        }
        settled
    }

    /// Wrap an opened interaction. `approvals` is the receiving end of the channel approver
    /// the interaction was opened with. Must be called inside a tokio runtime.
    pub async fn new(
        mut ix: Interaction,
        approvals: mpsc::UnboundedReceiver<PendingApproval>,
        info: UiInfo,
        handlers: HashMap<String, Handler>,
        transcription: Option<Arc<dyn crate::transcription::TranscriptionProvider>>,
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
        // Uploads a client left more than a day ago (ADR 0030).
        crate::upload::sweep(ix.ark().shared_engine().store.root());
        let resource_subscribers = Arc::new(ResourceSubscribers::default());
        let host = Arc::new(Host {
            orch: Mutex::new(None),
            orchs: ix.orchestrators(),
            terminals: crate::terminal::Terminals::new(resource_subscribers.clone()),
            resource_subscribers,
            file_snapshots: crate::file_snapshots::Snapshots::new(),
            project_resource_routes: Mutex::new(HashMap::new()),
            engine: ix.ark().shared_engine(),
            chat_skills: ix.ark().skill_names().to_vec(),
            caps: ark_interact::capabilities::Parts::of(ix.ark()),
            labels: ix.labels().clone(),
            commands: serde_json::to_value(ix.commands()).unwrap_or_default(),
            siblings: ix.siblings(),
            computer: ix.ark().computer().cloned(),
            processes: ix.ark().processes().cloned(),
            browser: ix.ark().browser_service().cloned(),
            workers: ix.ark().workers().cloned(),
            notifier: Arc::new(crate::notify::Notifier::new(ix.ark().shared_engine())),
            links: ix.links().clone(),
            held: Mutex::new(None),
            held_live: Mutex::new(HashMap::new()),
            relays: Mutex::new(HashMap::new()),
            current: Mutex::new(Conv::new(ix)),
            switching: tokio::sync::Mutex::new(()),
            client_mutations: tokio::sync::Mutex::new(()),
            background: Mutex::new(Vec::new()),
            addressed: Mutex::new(HashMap::new()),
            setup_terms: Mutex::new(Default::default()),
            worker_setup_terms: Mutex::new(Default::default()),
            worker_setup_owners: Mutex::new(Default::default()),
            copy_locks: Mutex::new(Default::default()),
            computers: Default::default(),
            feed: Feed::new(),
            approvals: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            snapshot: Mutex::new(Snapshot::default()),
            handlers,
            transcription,
            dictations: Mutex::new(HashMap::new()),
            info,
        });
        host.refresh().await;
        let first = Arc::downgrade(&host.cur());
        tokio::spawn(forward_approvals(host.clone(), approvals, first));
        tokio::spawn(forward_attention(
            Arc::downgrade(&host),
            host.orchs.subscribe(),
        ));
        // What waited before this process started (kept questions, results) is looked at too.
        host.notify_soon();
        // A linked runtime's list changed: windows read it again.
        tokio::spawn(forward_links(Arc::downgrade(&host), host.links.subscribe()));
        tokio::spawn(forward_project_terminal_exits(
            Arc::downgrade(&host),
            host.feed.subscribe(),
        ));
        let weak = Arc::downgrade(&host);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let Some(host) = weak.upgrade() else { break };
                host.sweep_dictations();
            }
        });
        // Chats held on other computers: their events, once their links are set.
        host.start_relays();
        if let Some(c) = &host.computer {
            tokio::spawn(forward_computer(Arc::downgrade(&host), c.watch()));
            tokio::spawn(forward_computer_activity(
                Arc::downgrade(&host),
                c.watch_activity(),
            ));
        }
        if let Some(p) = &host.processes {
            tokio::spawn(forward_processes(Arc::downgrade(&host), p.subscribe()));
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

    /// Whether windows show `conv`: not while they show a chat held elsewhere.
    fn is_current(&self, conv: &Arc<Conv>) -> bool {
        self.held.lock().unwrap().is_none() && Arc::ptr_eq(&self.cur(), conv)
    }

    /// The live conversation of `session`: the current one or one working in the background.
    fn conv_of(&self, session: &str) -> Option<Arc<Conv>> {
        let cur = self.cur();
        if cur.session() == session {
            return Some(cur);
        }
        let bg = self
            .background
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.session() == session)
            .cloned();
        bg.or_else(|| self.addressed.lock().unwrap().get(session).cloned())
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
        let cur = self.cur();
        let mut v: Vec<Value> = cur.working().into_iter().collect();
        let bg: Vec<Arc<Conv>> = self.background.lock().unwrap().clone();
        v.extend(bg.iter().filter_map(|c| c.working()));
        // A chat held for another runtime that is also current or in the background is listed once.
        let shown = |c: &Arc<Conv>| Arc::ptr_eq(c, &cur) || bg.iter().any(|b| Arc::ptr_eq(b, c));
        let addressed: Vec<Arc<Conv>> = self.addressed.lock().unwrap().values().cloned().collect();
        v.extend(
            addressed
                .iter()
                .filter(|c| !shown(c))
                .filter_map(|c| c.working()),
        );
        v.extend(self.held_working());
        v
    }

    /// Files of a project changed without a tool call (a restore, files added): open views and
    /// lists read them again.
    fn files_changed(&self, project: &ProjectId, paths: &[String]) {
        let references: HashMap<ResourceLocation, ResourceRef> = self
            .resource_subscribers
            .aliases(ResourceKind::File, project.as_str(), "*")
            .into_iter()
            .chain(paths.iter().flat_map(|path| {
                self.resource_subscribers
                    .aliases(ResourceKind::File, project.as_str(), path)
            }))
            .map(|reference| (reference.location.clone(), reference))
            .collect();
        for reference in references.into_values() {
            self.emit(json!({ "kind": "files_changed", "project": project.as_str(), "paths": paths, "session": reference.location.session, "ref": reference }));
        }
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
    /// Whether agents may use this machine's screen (the Computer).
    pub fn uses_computer(&self) -> bool {
        self.computer.is_some()
    }

    /// Take the screen back from Computer without going through the current page.
    /// Native overlays and the menu bar use this even while another app is in front.
    pub fn computer_stop(&self) -> Option<String> {
        self.computer
            .as_ref()
            .and_then(|computer| computer.take_back())
    }

    /// Remember the foreground app for Ambient Desktop only while this window shows a chat
    /// running here. A held SSH or Cloud chat never samples this computer.
    pub fn remember_front_app(&self) -> Result<Option<String>, String> {
        if self.shown_held().is_some() {
            return Ok(None);
        }
        match &self.computer {
            Some(computer) => computer.remember_front_app(),
            None => Ok(None),
        }
    }

    /// The runtime-owned Computer switch and volatile app-name projection.
    pub fn computer_context(&self) -> Option<ark_sdk::prelude::ark_computer::ComputerContext> {
        self.computer.as_ref().map(|computer| computer.context())
    }

    /// Re-read a message draft after the person has checked the app themselves.
    pub async fn computer_check(&self) -> Result<Value, String> {
        let computer = self
            .computer
            .as_ref()
            .ok_or_else(|| "this app does not use the computer".to_string())?;
        serde_json::to_value(computer.check_message_again().await?)
            .map_err(|error| error.to_string())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.feed.subscribe()
    }

    pub(crate) fn client_event_replay(&self, after: u64) -> Option<Vec<Value>> {
        self.feed.replay_after(after)
    }

    pub(crate) fn client_terminal_ref(
        &self,
        owner: &str,
        handle: &str,
        generation: &str,
    ) -> anyhow::Result<ResourceRef> {
        let routes = self.project_resource_routes.lock().unwrap();
        let kept = routes
            .get(handle)
            .ok_or_else(|| anyhow::anyhow!("that project Terminal route ended"))?;
        anyhow::ensure!(
            kept.owner.as_deref() == Some(owner),
            "that project Terminal belongs to another device"
        );
        anyhow::ensure!(
            kept.generation == generation,
            "that project Terminal generation changed"
        );
        anyhow::ensure!(
            !kept.terminal.is_empty(),
            "that project Terminal did not open"
        );
        Ok(self.project_route_ref(handle, &kept.runtime, &kept.local_project, &kept.terminal))
    }

    pub(crate) fn client_owns_terminal(&self, owner: &str, handle: &str) -> bool {
        self.project_resource_routes
            .lock()
            .unwrap()
            .get(handle)
            .is_some_and(|route| route.owner.as_deref() == Some(owner))
    }

    pub(crate) fn forget_client_terminal_routes(&self, owner: &str) {
        let references = {
            let routes = self.project_resource_routes.lock().unwrap();
            routes
                .iter()
                .filter(|(_, route)| route.owner.as_deref() == Some(owner))
                .map(|(handle, route)| {
                    self.project_route_ref(
                        handle,
                        &route.runtime,
                        &route.local_project,
                        &route.terminal,
                    )
                })
                .collect::<Vec<_>>()
        };
        self.project_resource_routes
            .lock()
            .unwrap()
            .retain(|_, route| route.owner.as_deref() != Some(owner));
        for reference in references {
            self.resource_subscribers.unsubscribe(&reference);
        }
    }

    pub(crate) fn client_mutation_counter(&self, device: &str, epoch: &str) -> anyhow::Result<u64> {
        let id = format!("{device}:{epoch}");
        Ok(self
            .engine
            .store
            .get_record("client_mutation_highwater", &id)?
            .and_then(|value| value["counter"].as_str()?.parse().ok())
            .unwrap_or(0))
    }

    pub(crate) async fn apply_client_mutation(
        self: &Arc<Self>,
        device: &str,
        request: &ark_protocol::MutationRequest,
    ) -> Result<ark_protocol::MutationReceipt, ClientMutationError> {
        match request.operation.as_str() {
            "chat.send"
                if request.input["body"]
                    .as_str()
                    .is_some_and(|body| !body.trim().is_empty()) => {}
            "attention.answer" if request.input["choiceId"].as_str().is_some() => {}
            "chat.send" | "attention.answer" => return Err(ClientMutationError::Unsupported),
            _ => return Err(ClientMutationError::Unsupported),
        }
        let _serial = self.client_mutations.lock().await;
        let counter = request
            .counter
            .as_str()
            .parse::<u64>()
            .map_err(|_| ClientMutationError::InvalidCounter)?;
        let record_id = format!("{device}:{}:{counter}", request.epoch);

        let existing = self
            .engine
            .store
            .get_record("client_mutation", &record_id)
            .map_err(|error| ClientMutationError::Store(error.into()))?;
        let mut record = if let Some(record) = existing {
            let same = record["operation"] == request.operation
                && record["target"] == request.target
                && record["inputDigest"] == request.input_digest;
            if !same {
                return Err(ClientMutationError::CounterConflict);
            }
            let receipt: ark_protocol::MutationReceipt =
                serde_json::from_value(record["receipt"].clone())
                    .map_err(|error| ClientMutationError::Store(error.into()))?;
            if receipt.status != "accepted" {
                return Ok(receipt);
            }
            record
        } else {
            let highwater = self.client_mutation_counter(device, &request.epoch)?;
            if highwater > 0 {
                let pending_id = format!("{device}:{}:{highwater}", request.epoch);
                let pending = self
                    .engine
                    .store
                    .get_record("client_mutation", &pending_id)
                    .map_err(|error| ClientMutationError::Store(error.into()))?;
                if pending
                    .as_ref()
                    .and_then(|record| record["receipt"]["status"].as_str())
                    == Some("accepted")
                {
                    return Err(ClientMutationError::CounterGap);
                }
            }
            if counter <= highwater {
                return Err(ClientMutationError::AlreadyApplied);
            }
            if counter != highwater.saturating_add(1) {
                return Err(ClientMutationError::CounterGap);
            }

            let accepted_at = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|error| ClientMutationError::Store(error.into()))?;
            let receipt = ark_protocol::MutationReceipt {
                id: uuid::Uuid::new_v4().to_string(),
                status: "accepted".into(),
                accepted_at,
                display: "Accepted".into(),
            };
            let record = json!({
                "device": device,
                "epoch": request.epoch,
                "counter": request.counter.as_str(),
                "operation": request.operation,
                "target": request.target,
                "inputDigest": request.input_digest,
                "input": request.input,
                "receipt": receipt,
            });
            let highwater = json!({ "counter": request.counter.as_str() });
            self.engine
                .store
                .put_records(&[
                    ("client_mutation", &record_id, device, &record),
                    (
                        "client_mutation_highwater",
                        &format!("{device}:{}", request.epoch),
                        device,
                        &highwater,
                    ),
                ])
                .map_err(|error| ClientMutationError::Store(error.into()))?;
            record
        };

        let (status, display) = match request.operation.as_str() {
            "chat.send" => {
                self.apply_client_send(&record_id, device, request, &mut record)
                    .await?
            }
            "attention.answer" => {
                let id = request
                    .target
                    .parse::<u64>()
                    .map(Value::from)
                    .unwrap_or_else(|_| Value::String(request.target.clone()));
                match self
                    .call(
                        "approve",
                        json!({ "id": id, "decision": request.input["choiceId"] }),
                    )
                    .await
                {
                    Ok(_) => ("completed", "Recorded"),
                    Err(_) => ("no_longer_applicable", "Nothing changed"),
                }
            }
            _ => unreachable!("operations were validated before acceptance"),
        };
        let mut receipt: ark_protocol::MutationReceipt =
            serde_json::from_value(record["receipt"].clone())
                .map_err(|error| ClientMutationError::Store(error.into()))?;
        receipt.status = status.into();
        receipt.display = display.into();
        record["receipt"] = serde_json::to_value(&receipt)
            .map_err(|error| ClientMutationError::Store(error.into()))?;
        self.engine
            .store
            .put_record("client_mutation", &record_id, device, &record)
            .map_err(|error| ClientMutationError::Store(error.into()))?;
        Ok(receipt)
    }

    async fn apply_client_send(
        self: &Arc<Self>,
        record_id: &str,
        device: &str,
        request: &ark_protocol::MutationRequest,
        record: &mut Value,
    ) -> Result<(&'static str, &'static str), ClientMutationError> {
        let body = request.input["body"].as_str().unwrap_or_default();
        if let Some(index) = record["effect"]["turnIndex"].as_u64() {
            let turns = self
                .engine
                .store
                .list_session_turns(&request.target, None)
                .map_err(|error| ClientMutationError::Store(error.into()))?;
            if turns.iter().any(|turn| {
                turn["index"].as_u64() == Some(index) && turn["input"].as_str() == Some(body)
            }) {
                return Ok(("completed", "Sent"));
            }
        }

        let _quiet = self.switching.lock().await;
        let Ok(conv) = self.chat_conv(&request.target, "", true).await else {
            return Ok(("no_longer_applicable", "Nothing changed"));
        };
        if conv.is_running() {
            return Ok(("accepted", "Accepted"));
        }
        let turn_index = self
            .engine
            .store
            .get_session(&request.target)
            .map_err(|error| ClientMutationError::Store(error.into()))?
            .and_then(|session| session["turn_count"].as_u64())
            .ok_or_else(|| ClientMutationError::Store(anyhow::anyhow!("chat has no turn count")))?;
        if let Some(expected) = record["effect"]["turnIndex"].as_u64() {
            if expected != turn_index {
                return Ok(("no_longer_applicable", "Nothing changed"));
            }
        } else {
            record["effect"] = json!({ "turnIndex": turn_index });
            self.engine
                .store
                .put_record("client_mutation", record_id, device, record)
                .map_err(|error| ClientMutationError::Store(error.into()))?;
        }
        if self.start_turn_on(conv, body.to_string(), true).is_err() {
            return Ok(("no_longer_applicable", "Nothing changed"));
        }
        for _ in 0..100 {
            let turns = self
                .engine
                .store
                .list_session_turns(&request.target, Some(1))
                .map_err(|error| ClientMutationError::Store(error.into()))?;
            if turns.iter().any(|turn| {
                turn["index"].as_u64() == Some(turn_index) && turn["input"].as_str() == Some(body)
            }) {
                return Ok(("completed", "Sent"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok(("accepted", "Accepted"))
    }

    pub(crate) async fn resume_client_mutations(self: &Arc<Self>) -> anyhow::Result<()> {
        for record in self.engine.store.list_records("client_mutation", None)? {
            if record["receipt"]["status"].as_str() != Some("accepted") {
                continue;
            }
            let Some(device) = record["device"].as_str() else {
                continue;
            };
            let request = ark_protocol::MutationRequest {
                epoch: record["epoch"].as_str().unwrap_or_default().to_string(),
                counter: ark_protocol::DecimalString::new(
                    record["counter"].as_str().unwrap_or_default(),
                )
                .map_err(anyhow::Error::msg)?,
                operation: record["operation"].as_str().unwrap_or_default().to_string(),
                target: record["target"].as_str().unwrap_or_default().to_string(),
                input_digest: record["inputDigest"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                input: record["input"].clone(),
            };
            let _ = self.apply_client_mutation(device, &request).await;
        }
        Ok(())
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

    /// The graph, each agent with `doing`: what it does now in words, as the terminal's
    /// `/agents` shows it (`ark_interact::orchestration::doing`).
    fn graph_with_doing(&self, g: &ark_interact::ark_orchestrate::Graph) -> anyhow::Result<Value> {
        let mut v = serde_json::to_value(g)?;
        if let Some(agents) = v["agents"].as_array_mut() {
            for (a, node) in agents.iter_mut().zip(&g.agents) {
                a["doing"] = json!(ark_interact::orchestration::doing(g, node, &self.labels));
            }
        }
        Ok(v)
    }

    /// What a phone's Home Screen shows for the app (`/manifest.webmanifest`): its name, its
    /// accent, its logo when it has one (ADR 0047).
    pub(crate) fn manifest(&self, name: &str) -> Value {
        let mut icons = Vec::new();
        if let Some(logo) = &self.info.logo {
            icons.push(json!({ "src": logo, "sizes": "any" }));
        }
        json!({
            "name": name,
            "short_name": name,
            "start_url": "/",
            "scope": "/",
            "display": "standalone",
            "theme_color": self.info.accent.clone().unwrap_or_else(|| "#ffffff".into()),
            "background_color": "#ffffff",
            "icons": icons,
        })
    }

    #[cfg(test)]
    pub(crate) fn engine_for_tests(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// Offer the person's own computers over SSH (`ArkUi::computers`): the ones added before are
    /// reached again.
    pub(crate) fn use_computers(
        self: &Arc<Self>,
        setup: crate::computers::ComputerSetup,
        keep: Vec<(String, String, String)>,
    ) {
        let c = crate::computers::Computers::new(setup, self.engine.clone(), self.links.clone());
        // What the app kept before (a server by address): a computer from now on.
        for (name, url, token) in keep {
            if let Err(e) = c.keep_address(&name, &url, &token) {
                tracing::warn!("computer {name}: {e:#}");
            }
        }
        c.start();
        let _ = self.computers.set(c);
    }

    /// The other runtimes whose items this one shows too (ADR 0043 step 4). The app sets them
    /// (`Links::set`, `ArkUi::link`): the people who reach this runtime act on those as its owner.
    pub fn links(&self) -> Arc<ark_interact::links::Links> {
        self.links.clone()
    }

    /// What waits for the person now, as notices for their devices: the attention list (any
    /// project) and the questions chats ask while their turns wait (ADR 0047).
    fn notices(&self) -> Vec<ark_interact::attention::Notice> {
        let mut out: Vec<_> = ark_interact::attention::list(&self.engine, &self.labels)
            .iter()
            .map(ark_interact::attention::notice)
            .collect();
        let local: Vec<Value> = self
            .approvals
            .lock()
            .unwrap()
            .values()
            .map(|(shown, _)| shown.clone())
            .collect();
        for shown in local.iter().chain(self.held_questions().iter()) {
            // An agent's question is the list's (its task keeps it).
            if !shown["request"]["agent"].is_null() {
                continue;
            }
            let Some(session) = shown["session"].as_str() else {
                continue;
            };
            let chat = shown["chat"].as_str().unwrap_or("a chat");
            let at_ms = shown["asked_ms"].as_u64().unwrap_or(0);
            let words = format!(
                "{} {}",
                shown["title"].as_str().unwrap_or(""),
                shown["line"].as_str().unwrap_or("")
            );
            out.push(ark_interact::attention::Notice {
                id: format!("chat:{session}:{at_ms}"),
                title: ark_interact::fmt::clip(chat, 60),
                body: ark_interact::fmt::clip(words.trim(), 140),
                tag: format!("chat:{session}"),
                urgent: true,
                at_ms,
            });
        }
        out
    }

    /// Something may wait for the person: their devices hear of it once it has waited a while.
    pub(crate) fn notify_soon(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        self.notifier.soon(Arc::new(move || {
            me.upgrade().map(|h| h.notices()).unwrap_or_default()
        }));
    }

    /// The project an agent's task works in (`{id, name}`), so its question can say where it
    /// comes from when the window shows another project.
    fn task_project(&self, task: Option<&str>) -> Value {
        let Some(task) = task else { return Value::Null };
        self.orchs
            .list()
            .into_iter()
            .find(|o| o.task(task).is_some())
            .map(|o| json!({ "id": o.project().id, "name": o.project().name }))
            .unwrap_or(Value::Null)
    }

    /// Questions tasks keep whose run is not waiting live in this process (it restarted), in
    /// the shape `hello` lists questions. Their id is the question's.
    fn kept_questions(&self, live: &[Value]) -> Vec<Value> {
        let asked_live = |task: &str| live.iter().any(|q| q["request"]["task"] == json!(task));
        self.orchs
            .list()
            .iter()
            .flat_map(|o| {
                let p = o.project();
                let project = json!({ "id": p.id, "name": p.name });
                o.questions().into_iter().map(move |t| (t, project.clone()))
            })
            .filter(|(t, _)| !asked_live(&t.id))
            .filter_map(|(t, project)| {
                let q = t.question?;
                Some(json!({
                    "id": q.id,
                    "request": q.request,
                    "line": self.labels.approval_line(&q.request),
                    "title": ark_interact::approval_title(&q.request),
                    "choices": ark_interact::approval_choices(&q.request),
                    "session": Value::Null,
                    "chat": Value::Null,
                    "project": project,
                }))
            })
            .collect()
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

    async fn resource_orchestrator(
        self: &Arc<Self>,
        reference: &ResourceRef,
    ) -> anyhow::Result<Orchestrator> {
        let conv = self
            .chat_conv(&reference.location.session, "", true)
            .await?;
        let orchestrator = conv.about.lock().unwrap().orch.clone();
        orchestrator.ok_or_else(|| anyhow::anyhow!("this app runs a single agent"))
    }

    async fn control_on(
        &self,
        orchestrator: &Orchestrator,
        control: Control,
    ) -> anyhow::Result<ControlOutcome> {
        ark_interact::orchestration::control(orchestrator, control).await
    }

    /// The verifier and event log of a webhook route of a connected account (ADR 0062).
    pub(crate) fn webhook(
        &self,
        source: &str,
        route: &str,
    ) -> Option<(
        Arc<dyn ark_interact::ark_sdk::prelude::ark_tools::events::WebhookSource>,
        Arc<dyn ark_interact::ark_sdk::prelude::ark_tools::events::EventSink>,
    )> {
        let conns = self.caps.connections.as_ref()?;
        ark_interact::automations::webhook(conns, &self.engine, source, route)
    }

    /// The app's saved API keys, when it keeps connections.
    fn api_keys(&self) -> Option<ark_interact::ark_sdk::prelude::ark_connect::ApiKeys> {
        self.caps.connections.as_ref().map(|c| c.api_keys().clone())
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
        v["location"] = json!(self.location_of(&cur));
        v
    }

    fn location_of(&self, conv: &Arc<Conv>) -> ResourceLocation {
        let about = conv.about.lock().unwrap();
        ResourceLocation {
            runtime: ark_interact::held::runtime_id(&self.engine),
            project: about.project.clone(),
            session: about.session.clone(),
        }
    }

    /// Resolve a resource without changing the chat any window shows.
    fn resource_project(&self, r: &ResourceRef) -> anyhow::Result<Project> {
        let here = ark_interact::held::runtime_id(&self.engine);
        anyhow::ensure!(
            r.location.runtime == here,
            "that resource belongs to another runtime"
        );
        if let ResourceAuthority::Project { route } = &r.authority {
            anyhow::ensure!(
                r.kind == ResourceKind::Terminal,
                "that project route is not a Terminal"
            );
            anyhow::ensure!(
                r.location.session.is_empty(),
                "a project route cannot name a chat"
            );
            let routes = self.project_resource_routes.lock().unwrap();
            let kept = routes
                .get(route)
                .ok_or_else(|| anyhow::anyhow!("that project Terminal route ended"))?;
            anyhow::ensure!(
                kept.runtime == here,
                "that route belongs to another runtime"
            );
            anyhow::ensure!(
                kept.local_project == r.location.project,
                "that route belongs to another project"
            );
            anyhow::ensure!(
                kept.kind == r.kind,
                "that route belongs to another resource kind"
            );
            if !kept.terminal.is_empty() || !r.id.is_empty() {
                anyhow::ensure!(
                    kept.terminal == r.id,
                    "that route belongs to another Terminal"
                );
            }
            drop(routes);
            let project = r.location.project.clone();
            return self
                .engine
                .store
                .get_project(&ProjectId(project.clone()))?
                .ok_or_else(|| anyhow::anyhow!("there is no project `{project}` here"));
        }
        anyhow::ensure!(
            !r.location.session.is_empty(),
            "the resource has no opening chat"
        );
        let project = if let Some(conv) = self.conv_of(&r.location.session) {
            conv.about.lock().unwrap().project.clone()
        } else {
            self.engine
                .store
                .get_session(&r.location.session)?
                .and_then(|v| v["project"].as_str().map(str::to_string))
                .ok_or_else(|| anyhow::anyhow!("there is no chat `{}` here", r.location.session))?
        };
        anyhow::ensure!(
            project == r.location.project,
            "the resource's project does not match its chat"
        );
        self.engine
            .store
            .get_project(&ProjectId(project.clone()))?
            .ok_or_else(|| anyhow::anyhow!("there is no project `{project}` here"))
    }

    fn resource_path<'a>(&self, r: &'a ResourceRef, input: &'a Value) -> &'a str {
        if r.id.is_empty() {
            input["path"].as_str().unwrap_or("")
        } else {
            &r.id
        }
    }

    fn scoped_upload(&self, r: &ResourceRef, input: &Value) -> String {
        use sha2::Digest as _;

        let identity = format!(
            "{}-{}-{}",
            r.location.runtime,
            r.location.session,
            input["upload"].as_str().unwrap_or("")
        );
        format!("{:x}", sha2::Sha256::digest(identity.as_bytes()))
    }

    fn project_route_ref(
        &self,
        route: &str,
        runtime: &str,
        project: &str,
        terminal: &str,
    ) -> ResourceRef {
        ResourceRef {
            location: ResourceLocation {
                runtime: runtime.to_string(),
                project: project.to_string(),
                session: String::new(),
            },
            kind: ResourceKind::Terminal,
            id: terminal.to_string(),
            authority: ResourceAuthority::Project {
                route: route.to_string(),
            },
        }
    }

    fn end_project_terminal_routes(&self, runtime: &str, project: &str, terminal: &str) {
        let routed: Vec<(ResourceRef, Option<String>)> = self
            .project_resource_routes
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, kept)| {
                kept.runtime == runtime
                    && kept.local_project == project
                    && kept.kind == ResourceKind::Terminal
                    && kept.terminal == terminal
            })
            .map(|(route, kept)| {
                (
                    self.project_route_ref(
                        route,
                        &kept.runtime,
                        &kept.local_project,
                        &kept.terminal,
                    ),
                    kept.owner.clone(),
                )
            })
            .collect();
        let aliases = self
            .resource_subscribers
            .aliases(ResourceKind::Terminal, project, terminal);
        let mut references: HashMap<ResourceRef, Option<String>> = aliases
            .into_iter()
            .map(|reference| (reference, None))
            .collect();
        references.extend(routed);
        for (reference, owner) in references {
            self.feed.send(json!({
                "kind": "term",
                "session": reference.location.session,
                "ref": reference,
                "exit": true,
                "_client_owner": owner,
            }));
        }
        self.project_resource_routes
            .lock()
            .unwrap()
            .retain(|_, kept| {
                kept.runtime != runtime
                    || kept.local_project != project
                    || kept.kind != ResourceKind::Terminal
                    || kept.terminal != terminal
            });
        self.resource_subscribers
            .remove_resource(ResourceKind::Terminal, project, terminal);
    }

    /// Open a project's shell without opening or changing a chat (ADR 0060).
    async fn project_terminal_open(self: &Arc<Self>, params: &Value) -> anyhow::Result<Value> {
        let project_id = params["project"].as_str().unwrap_or("");
        let project = self
            .engine
            .store
            .get_project(&ProjectId(project_id.to_string()))?
            .ok_or_else(|| anyhow::anyhow!("there is no project `{project_id}` here"))?;
        anyhow::ensure!(
            project.workspace.is_some(),
            "choose a folder for this project first"
        );
        let place = params["place"].as_str().unwrap_or("");
        let route = uuid::Uuid::new_v4().simple().to_string();
        let here = ark_interact::held::runtime_id(&self.engine);
        let operation = ResourceOperation {
            kind: ResourceOperationKind::TerminalOpen,
            input: json!({
                "cols": params["cols"],
                "rows": params["rows"],
            }),
        };

        if place.is_empty() {
            let reference = self.project_route_ref(&route, &here, project.id.as_str(), "");
            self.project_resource_routes.lock().unwrap().insert(
                route.clone(),
                ProjectResourceRoute {
                    source_project: project.id.as_str().to_string(),
                    local_project: project.id.as_str().to_string(),
                    runtime: here,
                    place: None,
                    kind: ResourceKind::Terminal,
                    terminal: String::new(),
                    origin: None,
                    owner: params["_client_owner"].as_str().map(str::to_string),
                    generation: String::new(),
                    next_write: 1,
                },
            );
            return match self.resource_call(reference, operation, false).await {
                Ok(mut value) => {
                    if let Some(kept) = self.project_resource_routes.lock().unwrap().get_mut(&route)
                    {
                        kept.generation = value["generation"].as_str().unwrap_or("").to_string();
                    }
                    value["place"] = json!(params["here"].as_str().unwrap_or("This computer"));
                    value["project"] = json!({ "id": project.id, "name": project.name });
                    Ok(value)
                }
                Err(error) => {
                    self.project_resource_routes.lock().unwrap().remove(&route);
                    Err(error)
                }
            };
        }

        let shown = self
            .links
            .shown()
            .into_iter()
            .find(|candidate| candidate.name == place)
            .ok_or_else(|| anyhow::anyhow!("{place} is not set up here"))?;
        anyhow::ensure!(
            shown.state == ark_interact::links::State::Ok,
            "{place} is not answering"
        );
        anyhow::ensure!(shown.project_terminal, "update ARK on {place}");
        let runtime = self
            .links
            .runtime_id(place)
            .ok_or_else(|| anyhow::anyhow!("update ARK on {place}"))?;
        self.ensure_relay(place).await;
        let sent = self
            .copy_send(place, &project, params["big"].as_bool().unwrap_or(false))
            .await?;
        if !sent["ask"].is_null() || sent["busy"] == true {
            return Ok(sent);
        }
        anyhow::ensure!(
            sent["refused"].as_array().is_none_or(Vec::is_empty),
            "some project files could not be copied"
        );
        let mut identity = self.origin(place, &project);
        let state = self
            .links
            .chat(place, "copy_state", identity.clone())
            .await?;
        let remote_project = state["project"]["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("{place} did not identify the project copy"))?;
        let reference = self.project_route_ref(&route, &runtime, remote_project, "");
        identity["ref"] = serde_json::to_value(&reference)?;
        identity["operation"] = serde_json::to_value(&operation)?;
        let mut value = self
            .links
            .chat(place, "project_resource_open", identity)
            .await?;
        let opened_ref: ResourceRef = serde_json::from_value(value["ref"].clone())?;
        anyhow::ensure!(
            opened_ref.location.runtime == runtime,
            "{place} returned another runtime"
        );
        anyhow::ensure!(
            opened_ref.location.project == remote_project,
            "{place} returned another project"
        );
        anyhow::ensure!(
            opened_ref.kind == ResourceKind::Terminal,
            "{place} returned another resource"
        );
        anyhow::ensure!(
            opened_ref.authority == reference.authority,
            "{place} returned another route"
        );
        self.project_resource_routes.lock().unwrap().insert(
            route.clone(),
            ProjectResourceRoute {
                source_project: project.id.as_str().to_string(),
                local_project: remote_project.to_string(),
                runtime,
                place: Some(place.to_string()),
                kind: ResourceKind::Terminal,
                terminal: opened_ref.id.clone(),
                origin: None,
                owner: params["_client_owner"].as_str().map(str::to_string),
                generation: value["generation"].as_str().unwrap_or("").to_string(),
                next_write: 1,
            },
        );
        value["place"] = json!(place);
        value["project"] = json!({ "id": project.id, "name": project.name });
        Ok(value)
    }

    async fn resource_call(
        self: &Arc<Self>,
        r: ResourceRef,
        operation: ResourceOperation,
        from_link: bool,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(
            operation.kind.accepts(r.kind),
            "operation {:?} is not valid for {:?}",
            operation.kind,
            r.kind
        );
        let project = self.resource_project(&r)?;
        let workspace = self.engine.workspace_dir(&project.id)?;
        let input = operation.input;
        let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
        let n = |k: &str| input[k].as_u64();
        use ResourceOperationKind as O;
        match operation.kind {
            O::Subscribe => {
                let id = if r.id.is_empty() {
                    "*".to_string()
                } else {
                    r.id.clone()
                };
                self.resource_subscribers
                    .subscribe(r.kind, project.id.as_str(), &id, r);
                Ok(json!({}))
            }
            O::Unsubscribe => {
                self.resource_subscribers.unsubscribe(&r);
                if r.kind == ResourceKind::File {
                    self.file_snapshots.close_resource(&r);
                }
                Ok(json!({}))
            }
            O::TerminalOpen => {
                let size = |k: &str, d: u64| n(k).unwrap_or(d).clamp(2, 1000) as u16;
                let opened = self.terminals.open(
                    &self.feed,
                    project.id.as_str(),
                    workspace,
                    size("cols", 80),
                    size("rows", 24),
                )?;
                let mut value = serde_json::to_value(&opened)?;
                let mut opened_ref = r.clone();
                opened_ref.id = opened.session;
                if let ResourceAuthority::Project { route } = &opened_ref.authority {
                    let mut routes = self.project_resource_routes.lock().unwrap();
                    let kept = routes
                        .get_mut(route)
                        .ok_or_else(|| anyhow::anyhow!("that project Terminal route ended"))?;
                    anyhow::ensure!(
                        kept.terminal.is_empty() || kept.terminal == opened_ref.id,
                        "that route belongs to another Terminal"
                    );
                    kept.terminal = opened_ref.id.clone();
                }
                self.terminals
                    .subscribe(project.id.as_str(), &opened_ref.id, opened_ref.clone());
                value["ref"] = serde_json::to_value(opened_ref)?;
                Ok(value)
            }
            O::TerminalRead => {
                if self.owns_setup_terminal(&r.id, &r.location.session) {
                    return Ok(serde_json::to_value(self.terminals.read_owned(
                        &r.id,
                        &s("generation"),
                        n("after").unwrap_or(0),
                    )?)?);
                }
                self.terminals
                    .subscribe(project.id.as_str(), &r.id, r.clone());
                let read = self.terminals.read_for(
                    project.id.as_str(),
                    &r.id,
                    &s("generation"),
                    n("after").unwrap_or(0),
                )?;
                let ended = read.ended;
                let value = serde_json::to_value(read)?;
                if ended && matches!(&r.authority, ResourceAuthority::Project { .. }) {
                    self.end_project_terminal_routes(
                        &r.location.runtime,
                        &r.location.project,
                        &r.id,
                    );
                }
                Ok(value)
            }
            O::TerminalWrite => {
                if let ResourceAuthority::Project { route } = &r.authority {
                    let sequence = n("sequence").ok_or_else(|| {
                        anyhow::anyhow!("a project Terminal write needs a sequence")
                    })?;
                    let mut routes = self.project_resource_routes.lock().unwrap();
                    let kept = routes
                        .get_mut(route)
                        .ok_or_else(|| anyhow::anyhow!("that project Terminal route ended"))?;
                    anyhow::ensure!(
                        sequence == kept.next_write,
                        "that Terminal write is duplicate or out of order"
                    );
                    // Consume before I/O. A failed or uncertain write is never safe to retry.
                    kept.next_write = kept.next_write.saturating_add(1);
                }
                if self.owns_setup_terminal(&r.id, &r.location.session) {
                    self.terminals.write(&r.id, &s("data"))?;
                } else {
                    self.terminals
                        .write_for(project.id.as_str(), &r.id, &s("data"))?;
                }
                Ok(Value::Null)
            }
            O::TerminalResize => {
                let size = |k: &str| n(k).unwrap_or(24).clamp(2, 1000) as u16;
                if self.owns_setup_terminal(&r.id, &r.location.session) {
                    self.terminals.resize(&r.id, size("cols"), size("rows"))?;
                } else {
                    self.terminals.resize_for(
                        project.id.as_str(),
                        &r.id,
                        size("cols"),
                        size("rows"),
                    )?;
                }
                Ok(Value::Null)
            }
            O::TerminalClose => {
                if self.owns_setup_terminal(&r.id, &r.location.session) {
                    self.terminals.close(&r.id);
                    self.setup_terms.lock().unwrap().remove(&r.id);
                    self.worker_setup_terms
                        .lock()
                        .unwrap()
                        .retain(|_, terminal| terminal != &r.id);
                    self.worker_setup_owners.lock().unwrap().remove(&r.id);
                } else {
                    self.terminals.close_for(project.id.as_str(), &r.id)?;
                }
                if matches!(&r.authority, ResourceAuthority::Project { .. }) {
                    self.end_project_terminal_routes(
                        &r.location.runtime,
                        &r.location.project,
                        &r.id,
                    );
                }
                Ok(Value::Null)
            }
            O::Processes => match &self.processes {
                Some(p) => {
                    self.resource_subscribers.subscribe(
                        ResourceKind::Process,
                        project.id.as_str(),
                        "*",
                        r,
                    );
                    Ok(json!(p.list_for(&workspace)))
                }
                None => anyhow::bail!("this app does not start processes"),
            },
            O::ProcessOutput => match self
                .processes
                .as_ref()
                .and_then(|p| p.raw_output_for(&workspace, &r.id))
            {
                Some(out) => Ok(json!({ "data": out })),
                None => anyhow::bail!("there is no process `{}` in this project", r.id),
            },
            O::ProcessStop => match &self.processes {
                Some(p) => match p.stop_for(&workspace, &r.id).await {
                    Some(info) => Ok(json!(info)),
                    None => anyhow::bail!("there is no process `{}` in this project", r.id),
                },
                None => anyhow::bail!("this app does not start processes"),
            },
            O::Files => {
                self.resource_subscribers.subscribe(
                    ResourceKind::File,
                    project.id.as_str(),
                    "*",
                    r.clone(),
                );
                let (entries, more) =
                    crate::files::list(&workspace, self.resource_path(&r, &input))?;
                Ok(json!({ "root": workspace, "entries": entries, "more": more }))
            }
            O::FileRead => {
                self.resource_subscribers.subscribe(
                    ResourceKind::File,
                    project.id.as_str(),
                    &r.id,
                    r.clone(),
                );
                let path = self.resource_path(&r, &input).to_string();
                let source = input["source"].as_bool().unwrap_or(false);
                tokio::task::spawn_blocking(move || {
                    if source {
                        let mut page = ark_files::source(&workspace, &path)?.page();
                        if ark_files::view_only(&path).is_some() {
                            page["read_only"] = json!(true);
                        }
                        Ok(page)
                    } else {
                        crate::files::read(&workspace, &path)
                    }
                })
                .await?
            }
            O::FileWrite => {
                let base = Some(s("base")).filter(|b| !b.is_empty());
                let value = crate::files::write(
                    &workspace,
                    self.resource_path(&r, &input),
                    &s("text"),
                    base.as_deref(),
                )?;
                self.files_changed(&project.id, std::slice::from_ref(&r.id));
                Ok(value)
            }
            O::FileEdit => {
                let base = Some(s("base")).filter(|b| !b.is_empty());
                let value = crate::files::edit(
                    &workspace,
                    self.resource_path(&r, &input),
                    input.get("edits").unwrap_or(&Value::Null),
                    base.as_deref(),
                )?;
                self.files_changed(&project.id, std::slice::from_ref(&r.id));
                Ok(value)
            }
            O::FileMedia => crate::files::media(&workspace, self.resource_path(&r, &input)),
            O::FileSnapshotOpen => self.file_snapshots.begin(workspace, r),
            O::FileSnapshotStatus => {
                let snapshots = self.file_snapshots.clone();
                let request = s("request");
                tokio::task::spawn_blocking(move || snapshots.status(&r, &request)).await?
            }
            O::FileSnapshotCancel | O::FileSnapshotClose => {
                let id = s(if operation.kind == O::FileSnapshotCancel {
                    "request"
                } else {
                    "snapshot"
                });
                self.file_snapshots.close(&r, &id)
            }
            O::FileSnapshotChunk => {
                let snapshots = self.file_snapshots.clone();
                let id = s("snapshot");
                let offset =
                    n("offset").ok_or_else(|| anyhow::anyhow!("a chunk needs an offset"))?;
                tokio::task::spawn_blocking(move || snapshots.chunk(&r, &id, offset)).await?
            }
            O::FileProbe => {
                let path = r.id;
                tokio::task::spawn_blocking(move || {
                    let (size, change) = ark_files::snapshot::probe(&workspace, &path)?;
                    Ok(json!({ "size": size, "change": change }))
                })
                .await?
            }
            O::FileVersion => crate::files::version(&workspace, self.resource_path(&r, &input)),
            O::FileRestore => {
                let restored = self
                    .engine
                    .restore_file(&project.id, &ArtifactId(s("artifact")))?;
                self.files_changed(&project.id, std::slice::from_ref(&restored.path));
                Ok(serde_json::to_value(restored)?)
            }
            O::UploadParts => {
                let store = self.engine.store.root().to_path_buf();
                let id = self.scoped_upload(&r, &input);
                let parts = input.get("parts").cloned().unwrap_or(Value::Null);
                tokio::task::spawn_blocking(move || crate::upload::put(&store, &id, &parts)).await?
            }
            O::UploadAdd => {
                let store = self.engine.store.root().to_path_buf();
                let id = self.scoped_upload(&r, &input);
                let dir = workspace.clone();
                let answer =
                    tokio::task::spawn_blocking(move || crate::upload::add(&store, &id, &dir))
                        .await??;
                let added: Vec<String> = serde_json::from_value(answer["added"].clone())?;
                self.files_changed(&project.id, &added);
                Ok(answer)
            }
            O::UploadCancel => {
                let store = self.engine.store.root().to_path_buf();
                let id = self.scoped_upload(&r, &input);
                tokio::task::spawn_blocking(move || crate::upload::cancel(&store, &id)).await??;
                Ok(json!({}))
            }
            O::FileOpen => {
                anyhow::ensure!(
                    !from_link,
                    "files on that computer cannot open in a local app"
                );
                crate::files::open_in_app(&workspace, self.resource_path(&r, &input))?;
                Ok(Value::Null)
            }
            O::Workspace => {
                let path = s("path");
                if !path.trim().is_empty() {
                    let conv = self.chat_conv(&r.location.session, "", true).await?;
                    anyhow::ensure!(!conv.is_running(), "that chat is working");
                    let mut ix = conv.ix.lock().await;
                    ix.set_workspace(std::path::Path::new(path.trim()))?;
                    conv.note(&ix);
                }
                let refreshed = self
                    .engine
                    .store
                    .get_project(&project.id)?
                    .unwrap_or(project);
                let dir = self.engine.workspace_dir(&refreshed.id)?;
                Ok(json!({ "path": dir, "chosen": refreshed.workspace.is_some() }))
            }
            O::State => {
                let state = self
                    .engine
                    .store
                    .get_research_state(&project.id)?
                    .unwrap_or_default();
                let artifacts = self.engine.store.list_artifacts(&project.id)?;
                Ok(serde_json::to_value(StateView::build(
                    state,
                    STATE_DIGEST_ITEMS,
                    &artifacts,
                ))?)
            }
            O::Graph => {
                let o = self.resource_orchestrator(&r).await?;
                self.resource_subscribers.subscribe(
                    ResourceKind::Project,
                    project.id.as_str(),
                    "orch",
                    r,
                );
                self.graph_with_doing(&o.graph())
            }
            O::TaskTrace => {
                let o = self.resource_orchestrator(&r).await?;
                Ok(serde_json::to_value(o.task_trace(&s("task"))?)?)
            }
            O::Task => {
                let o = self.resource_orchestrator(&r).await?;
                let id = s("task");
                let task = o
                    .task(&id)
                    .ok_or_else(|| anyhow::anyhow!("no task `{id}`"))?;
                let trace = o.task_trace(&id)?;
                let timeline: Vec<Value> = trace
                    .iter()
                    .filter_map(|event| {
                        let (line, tone) = event_line(&event.kind)?;
                        Some(json!({
                            "seq": event.seq, "at_ms": event.at_ms,
                            "agent": event.kind.agent(), "line": line,
                            "tone": tone, "event": event,
                        }))
                    })
                    .collect();
                // The task's events as windows get them live (`orch`), for an app that draws a
                // task the live stream has passed. Bookkeeping stays out; the last ones are kept.
                let names = HashMap::new();
                let mut feed: Vec<Value> = trace
                    .into_iter()
                    .map(|event| orch_event(&self.labels, &names, event))
                    .filter(|m| !m["line"].is_null() || !m["activity"].is_null())
                    .collect();
                feed.drain(..feed.len().saturating_sub(TASK_FEED));
                Ok(json!({ "task": task, "timeline": timeline, "feed": feed }))
            }
            O::TaskCall => {
                let o = self.resource_orchestrator(&r).await?;
                let id = s("task");
                let task = o
                    .task(&id)
                    .ok_or_else(|| anyhow::anyhow!("no task `{id}`"))?;
                let wanted = s("call");
                // The task's runtime events read as one turn, so a call reads as a chat's does.
                let mut record = ark_interact::TurnRecord {
                    input: task.objective.clone(),
                    live: !matches!(
                        task.state,
                        ark_interact::ark_orchestrate::TaskState::Done
                            | ark_interact::ark_orchestrate::TaskState::Failed
                            | ark_interact::ark_orchestrate::TaskState::Cancelled
                    ),
                    ..Default::default()
                };
                let trace = o.task_trace(&id)?;
                let start = trace.first().map(|e| e.at_ms).unwrap_or(0);
                for e in &trace {
                    if let ark_interact::ark_orchestrate::EventKind::Agent { event, .. } = &e.kind {
                        record.push(e.at_ms.saturating_sub(start), event);
                    }
                }
                let trace = interpret(&record);
                let call = trace
                    .calls
                    .iter()
                    .find(|c| c.id == wanted)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("no call `{wanted}` in task `{id}`"))?;
                // The agent's evidence is kept in its own project (its scope).
                let scope = o
                    .agent(&task.owner)
                    .map(|a| ProjectId(a.scope))
                    .unwrap_or_else(|| project.id.clone());
                let inv = match &call.invocation {
                    Some(inv) => self.engine.store.get_invocation(&scope, inv)?,
                    None => None,
                };
                let entries: Vec<_> = trace
                    .entries
                    .into_iter()
                    .filter(|e| e.call == Some(call.n))
                    .collect();
                Ok(json!({ "call": call, "invocation": inv, "entries": entries }))
            }
            O::OrchEvents => {
                let o = self.resource_orchestrator(&r).await?;
                let names = HashMap::new();
                let limit = n("limit").unwrap_or(200).min(2000) as usize;
                Ok(Value::Array(
                    o.events(n("after").unwrap_or(0), limit)?
                        .into_iter()
                        .map(|event| orch_event(&self.labels, &names, event))
                        .collect(),
                ))
            }
            O::Agents => Ok(serde_json::to_value(
                self.resource_orchestrator(&r).await?.agents(),
            )?),
            O::SpawnAgent => {
                let o = self.resource_orchestrator(&r).await?;
                let spec: AgentSpec = serde_json::from_value(input["spec"].clone())?;
                match self.control_on(&o, Control::Spawn(Box::new(spec))).await? {
                    ControlOutcome::Agent(agent) => Ok(serde_json::to_value(*agent)?),
                    _ => Ok(json!({})),
                }
            }
            O::SubmitTask => {
                let o = self.resource_orchestrator(&r).await?;
                let task: TaskSpec = serde_json::from_value(input["task"].clone())?;
                match self
                    .control_on(
                        &o,
                        Control::Submit {
                            agent: s("agent"),
                            task,
                        },
                    )
                    .await?
                {
                    ControlOutcome::Submitted { task } => Ok(json!({ "task": task })),
                    _ => Ok(json!({})),
                }
            }
            O::CancelTask | O::RetryTask => {
                let o = self.resource_orchestrator(&r).await?;
                let control = if operation.kind == O::CancelTask {
                    Control::Cancel { task: s("task") }
                } else {
                    Control::Retry { task: s("task") }
                };
                self.control_on(&o, control).await?;
                Ok(json!({}))
            }
            O::Schedules => {
                let o = self.resource_orchestrator(&r).await?;
                self.resource_subscribers.subscribe(
                    ResourceKind::Project,
                    project.id.as_str(),
                    "orch",
                    r,
                );
                let chat = Some(s("session")).filter(|c| !c.is_empty());
                Ok(serde_json::to_value(
                    ark_interact::orchestration::schedules(&o, chat.as_deref()),
                )?)
            }
            O::Schedule => {
                let o = self.resource_orchestrator(&r).await?;
                let id = s("schedule");
                let rec = o
                    .schedule_record(&id)
                    .ok_or_else(|| anyhow::anyhow!("no schedule `{id}`"))?;
                let limit = n("limit").unwrap_or(20).min(50) as usize;
                Ok(json!({
                    "schedule": ark_interact::orchestration::schedule_view(&o, &rec),
                    "runs": ark_interact::orchestration::schedule_runs(&o, &id, limit),
                }))
            }
            O::Unschedule | O::SchedulePause | O::ScheduleResume | O::ScheduleRun => {
                let o = self.resource_orchestrator(&r).await?;
                let schedule = s("schedule");
                let control = match operation.kind {
                    O::Unschedule => Control::Unschedule { schedule },
                    O::SchedulePause => Control::PauseSchedule { schedule },
                    O::ScheduleResume => Control::ResumeSchedule { schedule },
                    O::ScheduleRun => Control::RunSchedule { schedule },
                    _ => unreachable!(),
                };
                match self.control_on(&o, control).await? {
                    ControlOutcome::Submitted { task } => Ok(json!({ "task": task })),
                    _ => Ok(json!({})),
                }
            }
            O::Automations => {
                let o = self.resource_orchestrator(&r).await?;
                self.resource_subscribers.subscribe(
                    ResourceKind::Project,
                    project.id.as_str(),
                    "orch",
                    r,
                );
                let chat = Some(s("session")).filter(|c| !c.is_empty());
                Ok(serde_json::to_value(
                    ark_interact::automations::automations(&o, chat.as_deref()),
                )?)
            }
            O::Automation => {
                let o = self.resource_orchestrator(&r).await?;
                let id = s("automation");
                let rec = o
                    .automation(&id)
                    .ok_or_else(|| anyhow::anyhow!("no automation `{id}`"))?;
                let limit = n("limit").unwrap_or(20).min(50) as usize;
                Ok(json!({
                    "automation": ark_interact::automations::event_view(&o, &rec),
                    "runs": ark_interact::automations::event_runs(&o, &id, limit),
                }))
            }
            O::AutomationPause
            | O::AutomationResume
            | O::AutomationStop
            | O::AutomationDiscard
            | O::AutomationReconcile
            | O::AutomationFromNow => {
                let o = self.resource_orchestrator(&r).await?;
                let automation = s("automation");
                let control = match operation.kind {
                    O::AutomationPause => Control::PauseAutomation { automation },
                    O::AutomationResume => Control::ResumeAutomation { automation },
                    O::AutomationStop => Control::StopAutomation { automation },
                    O::AutomationReconcile => Control::RecoverFeed {
                        automation,
                        from_now: false,
                    },
                    O::AutomationFromNow => Control::RecoverFeed {
                        automation,
                        from_now: true,
                    },
                    _ => Control::DiscardPending { automation },
                };
                self.control_on(&o, control).await?;
                Ok(json!({}))
            }
            O::Board => {
                let o = self.resource_orchestrator(&r).await?;
                self.resource_subscribers.subscribe(
                    ResourceKind::Project,
                    project.id.as_str(),
                    "orch",
                    r,
                );
                let items = o.board();
                let mut tasks = serde_json::Map::new();
                for id in items.iter().flat_map(|work| work.tasks.iter()) {
                    if let Some(task) = o.task(id) {
                        tasks.insert(
                            id.clone(),
                            json!({
                                "owner": task.owner, "state": task.state,
                                "objective": task.objective, "result": task.result,
                                "error": task.error, "outputs": task.outputs,
                                "finished_ms": task.finished_ms, "question": task.question,
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
            O::BoardAdd | O::BoardChange => {
                let o = self.resource_orchestrator(&r).await?;
                let control = if operation.kind == O::BoardAdd {
                    Control::AddWork {
                        title: s("title"),
                        change: serde_json::from_value(input.clone())?,
                    }
                } else {
                    Control::ChangeWork {
                        item: s("item"),
                        change: serde_json::from_value(input["change"].clone())?,
                    }
                };
                match self.control_on(&o, control).await? {
                    ControlOutcome::Work(work) => Ok(serde_json::to_value(*work)?),
                    _ => Ok(json!({})),
                }
            }
            O::BoardRemove | O::BoardGive => {
                let o = self.resource_orchestrator(&r).await?;
                let control = if operation.kind == O::BoardRemove {
                    Control::RemoveWork { item: s("item") }
                } else {
                    Control::GiveWork {
                        item: s("item"),
                        agent: s("agent"),
                    }
                };
                match self.control_on(&o, control).await? {
                    ControlOutcome::Submitted { task } => Ok(json!({ "task": task })),
                    _ => Ok(json!({})),
                }
            }
            O::SuspendAgent | O::ResumeAgent | O::TerminateAgent | O::MessageAgent => {
                let o = self.resource_orchestrator(&r).await?;
                let control = match operation.kind {
                    O::SuspendAgent => Control::Suspend { agent: s("agent") },
                    O::ResumeAgent => Control::Resume { agent: s("agent") },
                    O::TerminateAgent => Control::Terminate { agent: s("agent") },
                    O::MessageAgent => Control::Message {
                        agent: s("agent"),
                        text: s("text"),
                    },
                    _ => unreachable!(),
                };
                match self.control_on(&o, control).await? {
                    ControlOutcome::Messaged { delivered, .. } => {
                        Ok(json!({ "delivered": delivered }))
                    }
                    _ => Ok(json!({})),
                }
            }
            O::Profiles => Ok(Value::Array(
                self.resource_orchestrator(&r)
                    .await?
                    .profiles()
                    .iter()
                    .map(|p| json!({ "name": p.name, "summary": p.summary }))
                    .collect(),
            )),
            O::Grants => Ok(serde_json::to_value(
                ark_sdk::prelude::ark_runtime::lasting_grants(
                    &self.engine.store,
                    project.id.as_str(),
                ),
            )?),
            O::GrantRemove => Ok(json!({
                "removed": ark_sdk::prelude::ark_runtime::remove_lasting_grant(
                    &self.engine.store, project.id.as_str(), &s("class"),
                )
            })),
            O::Skills => Ok(serde_json::to_value(ark_interact::skills::list_for(
                &self.engine,
                &project.id,
                &self.chat_skills,
            ))?),
            O::Tools => {
                let mut actions =
                    ark_sdk::offered_tools(&self.engine, &self.caps.per_agent).actions();
                actions.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(Value::Array(
                    actions
                        .iter()
                        .map(|d| {
                            json!({
                                "name": d.name, "summary": d.summary, "risk": d.risk,
                            })
                        })
                        .collect(),
                ))
            }
            O::Usage => {
                let conv = self.chat_conv(&r.location.session, "", true).await?;
                anyhow::ensure!(!conv.is_running(), "usage updates when this turn finishes");
                let usage = conv.ix.lock().await.usage_view();
                Ok(json!({ "usage": usage, "running": false }))
            }
            O::Artifacts => {
                let arts = self.engine.store.list_artifacts(&project.id)?;
                Ok(Value::Array(
                    arts.iter()
                        .map(|artifact| {
                            let summary = ArtifactSummary::from(artifact);
                            let mut value = serde_json::to_value(&summary)?;
                            let mut reference = r.clone();
                            reference.id = summary.id.clone();
                            value["ref"] = serde_json::to_value(reference)?;
                            anyhow::Ok(value)
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?,
                ))
            }
            O::Artifact => {
                let mut value = self.artifact_in(&project.id, &r.id, &input)?;
                value["ref"] = serde_json::to_value(&r)?;
                if value["artifact"].is_object() {
                    value["artifact"]["ref"] = serde_json::to_value(&r)?;
                }
                Ok(value)
            }
            O::Invocation => {
                let id = r.id.trim_start_matches("inv:");
                Ok(serde_json::to_value(
                    self.engine.store.resolve_invocation(&project.id, id)?,
                )?)
            }
            O::Picture => {
                use base64::Engine as _;
                let id = r.id.trim_start_matches("inv:");
                anyhow::ensure!(
                    self.engine
                        .store
                        .resolve_invocation(&project.id, id)?
                        .is_some(),
                    "there is no invocation `{id}` in this project"
                );
                let picture = self
                    .engine
                    .picture(&r.id)
                    .ok_or_else(|| anyhow::anyhow!("that picture is no longer available"))?;
                Ok(json!({
                    "media_type": picture.media_type,
                    "data": base64::engine::general_purpose::STANDARD.encode(picture.bytes.as_slice()),
                }))
            }
            O::ComputerPicture => {
                use base64::Engine as _;
                let shot = self
                    .computer
                    .as_ref()
                    .and_then(|c| c.picture(&s("app")))
                    .ok_or_else(|| anyhow::anyhow!("no picture of {} yet", s("app")))?;
                Ok(json!({
                    "media_type": "image/png",
                    "data": base64::engine::general_purpose::STANDARD.encode(&shot.png),
                    "width": shot.width,
                    "height": shot.height,
                }))
            }
            O::RunAction => Ok(serde_json::to_value(
                self.engine
                    .run_action(
                        &project.id,
                        &s("action"),
                        input.get("input").cloned().unwrap_or(Value::Null),
                        input["approved"].as_bool().unwrap_or(false),
                    )
                    .await?,
            )?),
            O::Focus => {
                let conv = self.chat_conv(&r.location.session, "", true).await?;
                let focus: Option<Focus> = serde_json::from_value(input["focus"].clone()).ok();
                if let Some(focus) = &focus {
                    crate::files::validate_focus(focus)?;
                }
                *conv.focus.lock().unwrap() = Some(focus);
                Ok(json!({}))
            }
            O::Extension => {
                let name = s("name");
                anyhow::ensure!(
                    r.id == name,
                    "the extension reference does not match `{name}`"
                );
                let handler = self
                    .handlers
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("no handler named `{name}`"))?;
                let Handler::Project(handler) = handler else {
                    anyhow::bail!("`{name}` is window-local")
                };
                handler(
                    ProjectHandlerCtx {
                        engine: self.engine.clone(),
                        project,
                        emitter: self.emitter(),
                        links: self.links(),
                    },
                    input.get("params").cloned().unwrap_or(Value::Null),
                )
                .await
            }
            browser_op @ (O::BrowserOpen
            | O::BrowserSnapshot
            | O::BrowserNavigate
            | O::BrowserResize
            | O::BrowserPointer
            | O::BrowserKey
            | O::BrowserText
            | O::BrowserWheel
            | O::BrowserBack
            | O::BrowserReload
            | O::BrowserStop
            | O::BrowserClose) => {
                let surface = self
                    .browser
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Chromium is not available on that computer"))?
                    .surface(&r.location.session);
                match browser_op {
                    O::BrowserOpen | O::BrowserNavigate => {
                        surface.open(&s("url")).await.map_err(anyhow::Error::msg)?;
                        Ok(json!({}))
                    }
                    O::BrowserSnapshot => Ok(serde_json::to_value(
                        surface
                            .snapshot(
                                n("width").unwrap_or(1280) as u32,
                                n("height").unwrap_or(900) as u32,
                            )
                            .await
                            .map_err(anyhow::Error::msg)?,
                    )?),
                    O::BrowserResize => {
                        surface
                            .resize(
                                n("width").unwrap_or(1280) as u32,
                                n("height").unwrap_or(900) as u32,
                                input["scale"].as_f64().unwrap_or(1.0),
                            )
                            .await
                            .map_err(anyhow::Error::msg)?;
                        Ok(json!({}))
                    }
                    O::BrowserPointer => {
                        surface
                            .pointer(
                                &s("event"),
                                input["x"].as_f64().unwrap_or(0.0),
                                input["y"].as_f64().unwrap_or(0.0),
                                input["button"].as_str().unwrap_or("none"),
                                n("clicks").unwrap_or(1) as u8,
                            )
                            .await
                            .map_err(anyhow::Error::msg)?;
                        Ok(json!({}))
                    }
                    O::BrowserKey => {
                        surface
                            .key(
                                &s("event"),
                                &s("key"),
                                &s("code"),
                                n("modifiers").unwrap_or(0) as u8,
                            )
                            .await
                            .map_err(anyhow::Error::msg)?;
                        Ok(json!({}))
                    }
                    O::BrowserText => {
                        surface.text(&s("text")).await.map_err(anyhow::Error::msg)?;
                        Ok(json!({}))
                    }
                    O::BrowserWheel => {
                        surface
                            .wheel(
                                input["x"].as_f64().unwrap_or(0.0),
                                input["y"].as_f64().unwrap_or(0.0),
                                input["dx"].as_f64().unwrap_or(0.0),
                                input["dy"].as_f64().unwrap_or(0.0),
                            )
                            .await
                            .map_err(anyhow::Error::msg)?;
                        Ok(json!({}))
                    }
                    O::BrowserBack => {
                        surface.back().await.map_err(anyhow::Error::msg)?;
                        Ok(json!({}))
                    }
                    O::BrowserReload => {
                        surface.reload().await.map_err(anyhow::Error::msg)?;
                        Ok(json!({}))
                    }
                    O::BrowserStop => {
                        surface.stop().await.map_err(anyhow::Error::msg)?;
                        Ok(json!({}))
                    }
                    O::BrowserClose => {
                        surface.close().await.map_err(anyhow::Error::msg)?;
                        Ok(json!({}))
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    /// Lock the current interaction for a request that changes it; refuse while a turn runs.
    async fn idle(&self) -> anyhow::Result<tokio::sync::OwnedMutexGuard<Interaction>> {
        let conv = self.cur();
        if conv.is_running() {
            anyhow::bail!("the agent is working; wait for the turn to finish or stop it");
        }
        Ok(conv.ix.clone().lock_owned().await)
    }

    fn worker_offers(&self) -> Vec<ark_sdk::prelude::ark_runtime::worker::WorkerOffer> {
        self.workers
            .as_ref()
            .map(|workers| workers.offers())
            .unwrap_or_default()
    }

    fn owns_setup_terminal(&self, terminal: &str, session: &str) -> bool {
        self.setup_terms
            .lock()
            .unwrap()
            .get(terminal)
            .is_some_and(|owner| owner == session)
            || self
                .worker_setup_owners
                .lock()
                .unwrap()
                .get(terminal)
                .is_some_and(|owners| owners.contains(session))
    }

    fn open_worker_setup(&self, session: &str, params: &Value) -> anyhow::Result<Value> {
        let text = |key: &str| params[key].as_str().unwrap_or("");
        let workers = self
            .workers
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("this runtime offers no agent programs"))?;
        let setup = workers
            .setup(text("spec"), text("action"))
            .ok_or_else(|| anyhow::anyhow!("that agent program is not offered here"))?
            .map_err(anyhow::Error::msg)?;
        let size =
            |key: &str, default: u64| params[key].as_u64().unwrap_or(default).clamp(2, 1000) as u16;
        let record = self
            .engine
            .store
            .get_session(session)?
            .ok_or_else(|| anyhow::anyhow!("there is no chat `{session}` here"))?;
        let reference = ResourceRef {
            location: ResourceLocation {
                runtime: ark_interact::held::runtime_id(&self.engine),
                project: record["project"].as_str().unwrap_or_default().to_string(),
                session: session.to_string(),
            },
            kind: ResourceKind::Terminal,
            id: String::new(),
            authority: ResourceAuthority::Chat,
        };
        let spec = text("spec").to_string();
        // Hold this through process creation. Two clients asking at the same time must either
        // attach to the same terminal or see the one this call inserts.
        let mut worker_setups = self.worker_setup_terms.lock().unwrap();
        if let Some(existing) = worker_setups.get(&spec).cloned() {
            if let Some(opened) = self.terminals.opened(&existing) {
                if !opened.ended {
                    self.terminals.subscribe(
                        &reference.location.project,
                        &existing,
                        ResourceRef {
                            id: existing.clone(),
                            ..reference.clone()
                        },
                    );
                    self.worker_setup_owners
                        .lock()
                        .unwrap()
                        .entry(existing)
                        .or_default()
                        .insert(session.to_string());
                    let mut value = serde_json::to_value(&opened)?;
                    let mut attached = reference;
                    attached.id = opened.session;
                    value["ref"] = serde_json::to_value(attached)?;
                    return Ok(value);
                }
                self.terminals.close(&existing);
            }
            worker_setups.remove(&spec);
            self.worker_setup_owners.lock().unwrap().remove(&existing);
        }
        let opened = self.terminals.open_program(
            &self.feed,
            crate::terminal::Program {
                argv: &setup.argv,
                clear_env: setup.clear_env,
                env: &setup.env,
                cwd: setup.cwd,
                cols: size("cols", 80),
                rows: size("rows", 24),
                reference: reference.clone(),
            },
        )?;
        self.worker_setup_owners
            .lock()
            .unwrap()
            .entry(opened.session.clone())
            .or_default()
            .insert(session.to_string());
        worker_setups.insert(spec, opened.session.clone());
        let mut reference = reference;
        reference.id = opened.session.clone();
        let mut value = serde_json::to_value(opened)?;
        value["ref"] = serde_json::to_value(reference)?;
        Ok(value)
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
        // The person's chat may be held on another computer: its calls go there.
        if let Some(r) = self.held_route(method, &params).await {
            return r;
        }
        match method {
            "dictation_status" => {
                let mut state = match &self.transcription {
                    Some(provider) => serde_json::to_value(provider.state())?,
                    None => json!({ "status": "unavailable" }),
                };
                state["runtime"] = json!(ark_interact::held::runtime_id(&self.engine));
                Ok(state)
            }
            "dictation_begin" => self.dictation_begin(&params),
            "dictation_chunk" => self.dictation_chunk(&params),
            "dictation_cancel" => self.dictation_cancel(&params),
            "dictation_finish" => self.dictation_finish(&params).await,
            "hello" => {
                // The person's zone, as their browser says it: "at 9" is their 9 also when
                // this runtime runs elsewhere.
                if let Some(tz) = params.get("tz").and_then(Value::as_str) {
                    self.siblings.set_zone(Some(tz));
                }
                let cur = self.cur();
                let mut running = cur
                    .running
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|r| r.input.clone());
                // Everything up to `seq` is in this answer: stored turns through `summary`,
                // the running turn through `live`. A window ignores events at or below it.
                let (seq, mut live) = self.feed.position(&cur.session());
                // Read after `seq`: a question asked meanwhile is here and may also arrive as
                // an event (the window shows it once), never in neither.
                let mut pending: Vec<Value> = self
                    .approvals
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, (req, _))| json!({ "id": id, "request": req["request"], "line": req["line"], "title": req["title"], "choices": req["choices"], "session": req["session"], "chat": req["chat"], "project": req["project"] }))
                    .collect();
                pending.extend(self.kept_questions(&pending));
                pending.extend(self.held_questions());
                let mut summary = self.summary();
                if let Some(chat) = self.shown_held() {
                    (summary, running, live) = self.held_hello(&chat);
                }
                Ok(json!({
                    "summary": summary,
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
                    "project_terminal": true,
                    "file_snapshots": cfg!(unix),
                    // This runtime's store id: another runtime knows it by this (ADR 0052).
                    "runtime": ark_interact::held::runtime_id(&self.engine),
                }))
            }
            // The person takes the screen back from the agent using it: its next action there
            // fails and it carries on without it.
            "computer_stop" => match &self.computer {
                Some(c) => Ok(json!({ "stopped": c.take_back() })),
                None => anyhow::bail!("this app does not use the computer"),
            },
            "computer_check" => match &self.computer {
                Some(c) => Ok(serde_json::to_value(
                    c.check_message_again().await.map_err(anyhow::Error::msg)?,
                )?),
                None => anyhow::bail!("this app does not use the computer"),
            },
            "computer_context" => Ok(serde_json::to_value(self.computer_context())?),
            // Where a new chat can run (ADR 0052): this computer, by its name, and the person's
            // other computers with how each is.
            "places" => {
                let here = if cfg!(target_os = "macos") {
                    "This Mac"
                } else {
                    "This computer"
                };
                let places: Vec<Value> = self
                    .links()
                    .shown()
                    .into_iter()
                    .map(|l| json!({ "name": l.name, "state": l.state, "project_terminal": l.project_terminal }))
                    .collect();
                Ok(
                    json!({ "here": here, "places": places, "can_add": self.computers.get().is_some() }),
                )
            }
            // Settings › Computers (ADR 0052): the person's computers, adding one over SSH, and
            // taking one away.
            "computers" => {
                let c = self.computers.get();
                Ok(json!({
                    "can_add": c.is_some(),
                    "computers": c.map(|c| c.list()).unwrap_or_default(),
                }))
            }
            "computer_hosts" => match self.computers.get() {
                Some(c) => Ok(json!(c.hosts().await)),
                None => Ok(json!([])),
            },
            // A computer at an address, with a code made there.
            "computer_add" if s("target").starts_with("http") => {
                let c = self
                    .computers
                    .get()
                    .ok_or_else(|| anyhow::anyhow!("this app does not add computers"))?;
                let name = c.add_address(&s("target"), &s("code")).await?;
                self.emit(json!({ "kind": "computer_setup", "target": s("target"), "done": true, "name": name }));
                Ok(json!({ "name": name }))
            }
            "computer_add" => {
                let c = self
                    .computers
                    .get()
                    .ok_or_else(|| anyhow::anyhow!("this app does not add computers"))?;
                let token = format!("ark-{}", &ark_interact::held::runtime_id(&self.engine)[..8]);
                let me = Arc::downgrade(self);
                c.add(
                    &s("target"),
                    token,
                    Arc::new(move |v: Value| {
                        if let Some(h) = me.upgrade() {
                            h.emit(v);
                        }
                    }),
                )?;
                Ok(json!({ "started": true }))
            }
            "computer_remove" => {
                let c = self
                    .computers
                    .get()
                    .ok_or_else(|| anyhow::anyhow!("this app does not add computers"))?;
                let name = s("name");
                let sessions: Vec<String> = ark_interact::held::list(&self.engine, None)
                    .into_iter()
                    .filter(|c| c.place == name)
                    .map(|c| c.session)
                    .collect();
                c.remove(
                    &name,
                    params["delete_store"].as_bool().unwrap_or(false),
                    params["here_only"].as_bool().unwrap_or(false),
                )
                .await?;
                self.forget_place(&name, &sessions).await;
                self.emit(json!({ "kind": "attention", "linked": true }));
                Ok(json!({}))
            }
            // The command a setup step left for the person, in a terminal on this computer:
            // `ssh` asking about a host key, `sudo` to keep the service running after logout.
            "computer_terminal" => {
                let argv = self
                    .computers
                    .get()
                    .and_then(|c| c.terminal_for(&s("target")))
                    .ok_or_else(|| anyhow::anyhow!("nothing to answer for {}", s("target")))?;
                let home = ark_os::dirs::home().unwrap_or_else(std::env::temp_dir);
                let size = |k: &str, d: u64| n(k).unwrap_or(d).clamp(2, 1000) as u16;
                let reference = ResourceRef {
                    location: self.location_of(&self.cur()),
                    kind: ResourceKind::Terminal,
                    id: String::new(),
                    authority: ResourceAuthority::Chat,
                };
                let opened = self.terminals.open_program(
                    &self.feed,
                    crate::terminal::Program {
                        argv: &argv,
                        clear_env: false,
                        env: &[],
                        cwd: home,
                        cols: size("cols", 80),
                        rows: size("rows", 24),
                        reference: reference.clone(),
                    },
                )?;
                self.setup_terms
                    .lock()
                    .unwrap()
                    .insert(opened.session.clone(), reference.location.session.clone());
                let mut value = serde_json::to_value(&opened)?;
                let mut reference = reference;
                reference.id = opened.session;
                value["ref"] = serde_json::to_value(reference)?;
                Ok(value)
            }
            // Every chat running a turn now (`{session, title, project, input}`).
            "working" => Ok(Value::Array(self.working())),
            "summary" => Ok(self.summary()),
            // `ask: false`: nobody can answer this turn's questions (a one-shot client), so its
            // state-changing actions are refused, not asked.
            "send" => {
                let text = s("text");
                if text.trim().is_empty() {
                    anyhow::bail!("nothing to send");
                }
                let ask = params.get("ask").and_then(|v| v.as_bool()).unwrap_or(true);
                let _quiet = self.switching.lock().await;
                if params.get("execution_mode").is_some() {
                    let expected: ark_interact::ark_sdk::prelude::ark_core::ExecutionMode =
                        serde_json::from_value(params["execution_mode"].clone())?;
                    let revision = params["execution_mode_revision"].as_u64().unwrap_or(0);
                    let conv = self.cur();
                    let ix = conv.ix.lock().await;
                    let summary = ix.summary();
                    anyhow::ensure!(
                        summary.execution_mode == expected
                            && summary.execution_mode_revision == revision,
                        "the execution mode changed; review it and send again"
                    );
                }
                self.start_turn(text, ask)?;
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
            // What waits for the person in any project (ADR 0043): the same list as `/waiting`.
            "attention" => Ok(serde_json::to_value(ark_interact::attention::list(
                &self.engine,
                &self.labels,
            ))?),
            // Notifications to this device (ADR 0047). `_by` and `_origin` come from the server:
            // the token or session that asked, and the address the page has.
            "push_key" => Ok(json!({ "key": self.notifier.public_key() })),
            "push_add" => {
                let added = self.notifier.add(
                    &params["subscription"],
                    &s("name"),
                    params["_by"].as_str().unwrap_or("local"),
                    &s("_origin"),
                    params["renew"].as_bool().unwrap_or(false),
                )?;
                // Removed from another device: it stays removed until turned on here.
                Ok(json!({ "ok": true, "removed": !added }))
            }
            // Devices signed in with a code, and signing one out (ADR 0047).
            "devices_signed_in" => Ok(self.notifier.signed_in()),
            "device_sign_out" => Ok(json!(self.notifier.sign_out(&s("name"))?)),
            "push_remove" => Ok(json!(match params["id"].as_str() {
                Some(id) => self.notifier.remove_id(id),
                None => self.notifier.remove(&s("endpoint")),
            })),
            "push_devices" => Ok(self.notifier.listed()),
            "push_here" => {
                self.notifier
                    .in_view(&s("endpoint"), params["visible"].as_bool().unwrap_or(false));
                Ok(json!({ "ok": true }))
            }
            // This runtime's items and the linked runtimes' (ADR 0043 step 4), with what is wrong
            // with a link, if anything. `attention` stays this runtime's own: a runtime that
            // links this one reads it, and its devices hear only of its own items.
            "attention_all" => {
                let links = self.links();
                Ok(json!({
                    "items": ark_interact::attention::everywhere(&self.engine, &self.labels, &links),
                    "notes": ark_interact::attention::link_notes(&links, false),
                    "away": ark_interact::attention::away(&links),
                }))
            }
            // Answering a question on another runtime: there, by its kept question's id.
            "attention_answer" => {
                let decision: ApprovalDecision =
                    serde_json::from_value(params["decision"].clone())?;
                self.links()
                    .answer(&s("runtime"), &s("id"), decision)
                    .await?;
                Ok(json!({ "ok": true }))
            }
            // Opening an item where it is, in this window: that runtime's page, on the item. Only
            // a window can; a browser shows the item's words instead (`attention_open`).
            "attention_go" => {
                let (links, runtime) = (self.links(), s("runtime"));
                let (url, token) = links
                    .page(&runtime)
                    .ok_or_else(|| anyhow::anyhow!("{runtime} is not linked here"))?;
                crate::remote::visit_item(&url, &token, &s("id"))
                    .await
                    .map_err(|e| anyhow::anyhow!(links.words(&runtime, &e)))?;
                Ok(json!({}))
            }
            // A window shows this chat to the person: what was posted there is seen.
            "attention_seen" => {
                let n = ark_interact::attention::seen_chat(&self.engine, &s("session"));
                if n > 0 {
                    self.emit(json!({ "kind": "attention" }));
                }
                Ok(json!(n))
            }
            // Open or clear one item. Opening a result or a failure clears it; the window then
            // shows where it lives (its chat, or its agent's task).
            // On another runtime: there.
            "attention_open" | "attention_clear" if !s("runtime").is_empty() => {
                let links = self.links();
                if method == "attention_clear" {
                    links.clear(&s("runtime"), &s("id")).await?;
                    return Ok(json!({}));
                }
                links.open(&s("runtime"), &s("id")).await
            }
            "attention_open" | "attention_clear" => {
                let id = s("id");
                let item = ark_interact::attention::list(&self.engine, &self.labels)
                    .into_iter()
                    .find(|i| i.id == id)
                    .ok_or_else(|| anyhow::anyhow!("that is no longer waiting"))?;
                if !item.is_question() {
                    ark_interact::attention::clear(&self.engine, &item)?;
                    self.emit(json!({ "kind": "attention" }));
                } else if method == "attention_clear" {
                    anyhow::bail!("a question leaves when it is answered");
                }
                let text = ark_interact::attention::text(&self.engine, &item);
                let mut v = serde_json::to_value(item)?;
                v["text"] = json!(text);
                Ok(v)
            }
            "approve" => {
                let decision: ApprovalDecision =
                    serde_json::from_value(params["decision"].clone())?;
                // A question a task kept, whose run is gone (a restart): its id is a string.
                if let Some(question) = params["id"].as_str() {
                    let (o, task) =
                        self.orchs
                            .list()
                            .into_iter()
                            .find_map(|o| {
                                let t = o.questions().into_iter().find(|t| {
                                    t.question.as_ref().is_some_and(|q| q.id == question)
                                })?;
                                Some((o, t.id))
                            })
                            .ok_or_else(|| anyhow::anyhow!("that approval is no longer pending"))?;
                    o.answer(&task, decision)?;
                    self.emit(json!({ "kind": "approval_closed", "id": question }));
                    return Ok(json!({}));
                }
                let id = n("id").unwrap_or(0);
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
                    self.start_turn(text.clone(), true)?;
                }
                // `/resume` of a chat held on another computer: it opens there.
                if let Some(Effect::Open(id)) = &reply.effect {
                    if let Some(chat) = ark_interact::held::get(&self.engine, id) {
                        self.held_open(chat).await?;
                        return Ok(serde_json::to_value(reply)?);
                    }
                }
                // A list read while a held chat is shown leaves it shown.
                if self.held.lock().unwrap().is_none() {
                    self.emit(json!({ "kind": "summary", "summary": self.summary() }));
                }
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
                let held_shown = self.held.lock().unwrap().is_some();
                for row in v.as_array_mut().into_iter().flatten() {
                    if row["id"]
                        .as_str()
                        .is_some_and(|id| working.iter().any(|w| w == id))
                    {
                        row["working"] = json!(true);
                    }
                    if held_shown {
                        row["current"] = json!(false);
                    }
                }
                // Chats held on other computers, in the same list, newest first.
                let scope = if !project.is_empty() {
                    Some(project)
                } else if params.get("all").and_then(|v| v.as_bool()).unwrap_or(false) {
                    None
                } else {
                    Some(self.project()?.id.as_str().to_string())
                };
                let held = self.held_rows(scope.as_deref());
                if !held.is_empty() {
                    let rows = v.as_array_mut().expect("a list");
                    rows.extend(held);
                    rows.sort_by(|a, b| {
                        b["updated_at"]
                            .as_str()
                            .unwrap_or("")
                            .cmp(a["updated_at"].as_str().unwrap_or(""))
                    });
                }
                Ok(v)
            }
            "projects" => Ok(serde_json::to_value(
                ark_interact::interaction::list_projects(&self.engine, &self.project()?.id)?,
            )?),
            "models" => Ok(serde_json::to_value(
                ark_interact::ark_sdk::prelude::ark_model::catalog().await,
            )?),
            "worker_offers" => Ok(serde_json::to_value(self.worker_offers())?),
            "worker_setup" => self.open_worker_setup(&self.session_id(), &params),
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
            // WhatsApp Business numbers (ark-connect): the connected ones with the path Meta
            // sends their webhooks to, and connecting one after Graph confirms its setup.
            "whatsapp" => {
                let c = self.caps.connections.as_ref();
                Ok(json!({ "can_connect": c.is_some(), "numbers": whatsapp_numbers(c) }))
            }
            "whatsapp_connect" => {
                let c = self
                    .caps
                    .connections
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("this app connects no accounts"))?;
                let setup = ark_interact::ark_sdk::prelude::ark_connect::services::whatsapp::Setup {
                    waba_id: s("waba_id").trim().into(),
                    phone_id: s("phone_id").trim().into(),
                    app_secret: s("app_secret").trim().into(),
                    verify_token: s("verify_token").trim().into(),
                    token: s("token").trim().into(),
                };
                let secrets = [
                    setup.app_secret.clone(),
                    setup.verify_token.clone(),
                    setup.token.clone(),
                ];
                c.connect_whatsapp(setup)
                    .await
                    .map_err(|e| anyhow::anyhow!(without(&e.to_string(), &secrets)))?;
                Ok(json!({ "numbers": whatsapp_numbers(Some(c)) }))
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
            // A detached native window creates an addressed chat without changing any window's
            // runtime-wide foreground. It immediately opens that chat through `chat_read`.
            "window_new_session" => {
                let project = s("project");
                let project = if project.is_empty() {
                    self.project()?.id.as_str().to_string()
                } else {
                    project
                };
                self.create_client_chat(&project).await
            }
            "new_session" => {
                let project = s("project");
                self.switch(Target::Change, move |ix| {
                    if !project.is_empty() {
                        ix.open_project(&project)?;
                    }
                    ix.new_session()
                })
                .await
            }
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
                let opened = self
                    .switch(Target::Session(target.clone()), move |ix| {
                        if ix.resume(&id)? {
                            Ok(())
                        } else {
                            anyhow::bail!("no session matches `{id}`")
                        }
                    })
                    .await?;
                self.seen();
                Ok(opened)
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
            "execution_mode" => {
                let mode = serde_json::from_value(params["mode"].clone())?;
                let revision = n("revision").unwrap_or(0);
                let conv = self.cur();
                let mut ix = conv.ix.lock().await;
                ix.set_execution_mode(revision, mode)?;
                conv.note(&ix);
                drop(ix);
                let summary = self.summary_of(&conv);
                self.emit(json!({ "kind": "summary", "summary": summary }));
                Ok(summary)
            }
            "switch_project" => {
                let name = s("name");
                self.switch(Target::Change, move |ix| {
                    ix.switch_project(&name).map(|_| ())
                })
                .await
                .inspect(|_| self.seen())
            }
            "open_project" => {
                let id = s("id");
                self.switch(Target::Change, move |ix| ix.open_project(&id).map(|_| ()))
                    .await
                    .inspect(|_| self.seen())
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
            // The Browser panel in a plain browser: whether a page lets a frame show it.
            "frame_check" => Ok(crate::frame::check(&s("url")).await),
            "project_terminal_open" => self.project_terminal_open(&params).await,
            "resource" => {
                let reference: ResourceRef = serde_json::from_value(params["ref"].clone())?;
                let operation: ResourceOperation =
                    serde_json::from_value(params["operation"].clone())?;
                self.resource_call(reference, operation, false).await
            }
            // A folder from another computer becomes a project here (see `upload`, ADR 0030):
            // its files in parts, then the project. Neither waits for a running turn.
            "upload_project_parts" => {
                let (store, id) = (self.engine.store.root().to_path_buf(), s("upload"));
                let parts = params.get("parts").cloned().unwrap_or(Value::Null);
                tokio::task::spawn_blocking(move || crate::upload::put(&store, &id, &parts)).await?
            }
            "upload_project_finish" => {
                let (engine, id, name) = (self.engine.clone(), s("upload"), s("name"));
                tokio::task::spawn_blocking(move || crate::upload::finish(&engine, &id, &name))
                    .await?
            }
            "upload_project_cancel" => {
                let (store, id) = (self.engine.store.root().to_path_buf(), s("upload"));
                tokio::task::spawn_blocking(move || crate::upload::cancel(&store, &id)).await??;
                Ok(json!({}))
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
            // A capability the app offers and the person turns on or off (the Computer).
            "capability_turn" => {
                if s("id").eq_ignore_ascii_case(ark_sdk::prelude::ark_computer::ID)
                    && self.shown_held().is_some()
                {
                    anyhow::bail!(
                        "Computer belongs to the chat's runtime; this chat is running elsewhere"
                    );
                }
                ark_interact::capabilities::turn(
                    self.caps.computer.as_ref(),
                    &s("id"),
                    params
                        .get("on")
                        .and_then(|v| v.as_bool())
                        .ok_or_else(|| anyhow::anyhow!("capability_turn needs `on`"))?,
                )?;
                Ok(json!({}))
            }
            "capability_disconnect" => Ok(serde_json::to_value(
                ark_interact::capabilities::disconnect(self.caps.connections.as_ref(), &s("id"))
                    .await?,
            )?),
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
            // Marketplace installs are inert local records. Connecting an account is a distinct
            // managed-provider operation and will never be inferred from Add.
            "marketplace_plugins" => {
                if !ark_interact::plugins::supported(&self.engine) {
                    return Ok(Value::Null);
                }
                Ok(serde_json::to_value(ark_interact::plugins::marketplace(
                    &self.engine,
                ))?)
            }
            "marketplace_add" => Ok(serde_json::to_value(
                ark_interact::plugins::add_marketplace(self.engine.clone(), s("id")).await?,
            )?),
            "marketplace_remove" => {
                ark_interact::plugins::remove_marketplace(self.engine.clone(), s("id")).await?;
                Ok(json!({}))
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
            "ext" => {
                let name = s("name");
                let h = self
                    .handlers
                    .get(&name)
                    .ok_or_else(|| anyhow::anyhow!("no handler named `{name}`"))?;
                let Handler::Window(h) = h else {
                    anyhow::bail!("`{name}` is a project resource and needs a resource reference")
                };
                let ctx = WindowHandlerCtx {
                    engine: self.engine.clone(),
                    emitter: self.emitter(),
                    links: self.links(),
                };
                h(ctx, params.get("params").cloned().unwrap_or(Value::Null)).await
            }
            m if m.starts_with("chat_") => self.chat_call(m, &params).await,
            m if m.starts_with("copy_") => self.copy_call(m, &params).await,
            m if m.starts_with("project_resource") => self.chat_call(m, &params).await,
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

    /// The chat now shown has been seen: what was posted into it no longer waits. A chat that
    /// was live in the background is brought forward without `Interaction::resume`, and a
    /// project opens on its latest chat, so this runs after every move to another chat.
    fn seen(&self) {
        ark_interact::attention::seen_chat(&self.engine, &self.session_id());
        self.emit(json!({ "kind": "attention" }));
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
            if cur.about.lock().unwrap().project != before {
                // A report of what the person saw in the other project is not about this one.
                *cur.focus.lock().unwrap() = None;
            }
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
        // A window that opens a chat another runtime holds here takes the same conversation.
        let held = self.addressed.lock().unwrap().remove(session);
        let mut bg = self.background.lock().unwrap();
        match bg.iter().position(|c| c.session() == session) {
            Some(i) => Some(bg.remove(i)),
            None => held,
        }
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
        self.trace_of(&self.cur(), index).await
    }

    async fn trace_of(
        &self,
        conv: &Arc<Conv>,
        index: Option<u32>,
    ) -> anyhow::Result<Option<ark_interact::Trace>> {
        if let Some(i) = index {
            return Ok(load_trace(&self.engine, &conv.session(), i).map(|r| interpret(&r)));
        }
        if let Some(r) = conv.running.lock().unwrap().as_ref() {
            let rec = r.ctl.recorder.lock().unwrap().snapshot();
            return Ok(Some(interpret(&rec)));
        }
        let t = conv.ix.lock().await.trace(None);
        Ok(t)
    }

    /// A chat's context, for one that windows do not show (`chat_context`).
    async fn context_of(&self, conv: &Arc<Conv>) -> anyhow::Result<ContextView> {
        if let Some(r) = conv.running.lock().unwrap().as_ref() {
            let t = interpret(&r.ctl.recorder.lock().unwrap().snapshot());
            if let Some(report) = t.requests.last() {
                return Ok(ContextView::build(
                    report,
                    ContextSource::LastRequest,
                    None,
                    &KeptOut::default(),
                ));
            }
            anyhow::bail!("no context yet");
        }
        let ix = conv.ix.lock().await;
        ix.context_view().await
    }

    /// The live conversation of a chat another runtime reaches by name (`chat_*`): the one open
    /// here already, else opened beside the windows' without moving them.
    /// `model`: the chat's model as the other runtime keeps it, put back on a conversation
    /// opened again (one opened here takes this runtime's current model). `keep`: hold it open
    /// (a turn starts on it); else it is read and let go, and nothing here grows with the
    /// chats another runtime looks at.
    async fn chat_conv(
        self: &Arc<Self>,
        session: &str,
        model: &str,
        keep: bool,
    ) -> anyhow::Result<Arc<Conv>> {
        anyhow::ensure!(!session.is_empty(), "which chat?");
        if let Some(c) = self.conv_of(session) {
            return Ok(c);
        }
        let rec = self
            .engine
            .store
            .get_session(session)?
            .ok_or_else(|| anyhow::anyhow!("there is no chat `{session}` here"))?;
        let project = ProjectId(rec["project"].as_str().unwrap_or_default().to_string());
        let conv = self.open_sibling(Some(session), &project)?;
        if !model.is_empty() {
            let mut ix = conv.ix.lock().await;
            // A chat only read still opens when its model is gone here (on this computer's
            // own); a turn needs it.
            match ix.switch_model(model) {
                Ok(_) => conv.note(&ix),
                Err(e) if keep => return Err(e),
                Err(_) => {}
            }
        }
        if keep {
            self.addressed
                .lock()
                .unwrap()
                .insert(session.to_string(), conv.clone());
        }
        Ok(conv)
    }

    /// What `summary` answers for a chat that windows may not show.
    fn summary_of(&self, conv: &Arc<Conv>) -> Value {
        let mut v = conv.about.lock().unwrap().summary.clone();
        v["running"] = json!(conv.is_running());
        v["location"] = json!(self.location_of(conv));
        v
    }

    /// Make a chat for an external client without changing the chat shown in any window.
    pub(crate) async fn create_client_chat(
        self: &Arc<Self>,
        project: &str,
    ) -> anyhow::Result<Value> {
        let project = self
            .engine
            .store
            .get_project(&ProjectId(project.to_string()))?
            .ok_or_else(|| anyhow::anyhow!("there is no project `{project}` here"))?;
        let conv = self.open_sibling(None, &project.id)?;
        let session = conv.session();
        self.addressed.lock().unwrap().insert(session, conv.clone());
        Ok(self.summary_of(&conv))
    }

    /// The questions a chat asks now (`hello`'s shape).
    fn questions_of(&self, session: &str) -> Vec<Value> {
        self.approvals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, (q, _))| q["session"] == json!(session))
            .map(|(id, (q, _))| json!({ "id": id, "request": q["request"], "line": q["line"], "title": q["title"], "choices": q["choices"], "session": q["session"], "chat": q["chat"], "project": q["project"] }))
            .collect()
    }

    /// The calls another runtime makes on a chat it holds here (ADR 0052). Each names its chat;
    /// none moves this runtime's windows.
    async fn chat_call(self: &Arc<Self>, method: &str, params: &Value) -> anyhow::Result<Value> {
        let s = |k: &str| params[k].as_str().unwrap_or("").to_string();
        let session = s("session");
        match method {
            "project_resource_open" => {
                let reference: ResourceRef = serde_json::from_value(params["ref"].clone())?;
                let ResourceAuthority::Project { route } = &reference.authority else {
                    anyhow::bail!("a project resource needs project authority")
                };
                let route = route.clone();
                anyhow::ensure!(
                    reference.kind == ResourceKind::Terminal,
                    "only a Terminal may use project authority"
                );
                anyhow::ensure!(
                    reference.id.is_empty(),
                    "a new project route cannot name a Terminal"
                );
                anyhow::ensure!(
                    reference.location.session.is_empty(),
                    "a project route cannot name a chat"
                );
                let origin = s("origin");
                let source_project = s("origin_project");
                let project = ark_interact::held::origin_project(
                    &self.engine,
                    &origin,
                    &source_project,
                    &s("name"),
                )?;
                let here = ark_interact::held::runtime_id(&self.engine);
                anyhow::ensure!(
                    reference.location.runtime == here,
                    "that route names another runtime"
                );
                anyhow::ensure!(
                    reference.location.project == project.id.as_str(),
                    "that route names another project copy"
                );
                {
                    let mut routes = self.project_resource_routes.lock().unwrap();
                    anyhow::ensure!(
                        !routes.contains_key(&route),
                        "that project route already exists"
                    );
                    routes.insert(
                        route.clone(),
                        ProjectResourceRoute {
                            source_project,
                            local_project: project.id.as_str().to_string(),
                            runtime: here,
                            place: None,
                            kind: ResourceKind::Terminal,
                            terminal: String::new(),
                            origin: Some(origin),
                            owner: None,
                            generation: String::new(),
                            next_write: 1,
                        },
                    );
                }
                let operation: ResourceOperation =
                    serde_json::from_value(params["operation"].clone())?;
                anyhow::ensure!(
                    operation.kind == ResourceOperationKind::TerminalOpen,
                    "a new project route must open a Terminal"
                );
                match self.resource_call(reference, operation, true).await {
                    Ok(value) => Ok(value),
                    Err(error) => {
                        self.project_resource_routes.lock().unwrap().remove(&route);
                        Err(error)
                    }
                }
            }
            "project_resource" => {
                let reference: ResourceRef = serde_json::from_value(params["ref"].clone())?;
                let ResourceAuthority::Project { route } = &reference.authority else {
                    anyhow::bail!("a project resource needs project authority")
                };
                let origin = s("origin");
                let source_project = s("origin_project");
                {
                    let routes = self.project_resource_routes.lock().unwrap();
                    let kept = routes
                        .get(route)
                        .ok_or_else(|| anyhow::anyhow!("that project Terminal route ended"))?;
                    anyhow::ensure!(
                        kept.origin.as_deref() == Some(origin.as_str()),
                        "that route belongs to another origin"
                    );
                    anyhow::ensure!(
                        kept.source_project == source_project,
                        "that route belongs to another source project"
                    );
                }
                let operation: ResourceOperation =
                    serde_json::from_value(params["operation"].clone())?;
                self.resource_call(reference, operation, true).await
            }
            "chat_resource" => {
                let reference: ResourceRef = serde_json::from_value(params["ref"].clone())?;
                anyhow::ensure!(
                    reference.location.session == session,
                    "the resource belongs to another chat"
                );
                let operation: ResourceOperation =
                    serde_json::from_value(params["operation"].clone())?;
                self.resource_call(reference, operation, true).await
            }
            "chat_forget_resources" => {
                self.resource_subscribers.remove_session(&session);
                let setup: Vec<String> = self
                    .setup_terms
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, owner)| *owner == &session)
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in setup {
                    self.terminals.close(&id);
                    self.setup_terms.lock().unwrap().remove(&id);
                }
                self.worker_setup_owners
                    .lock()
                    .unwrap()
                    .retain(|_, owners| {
                        owners.remove(&session);
                        !owners.is_empty()
                    });
                if let Some(browser) = &self.browser {
                    let _ = browser.surface(&session).close().await;
                }
                Ok(json!({ "forgotten": true }))
            }
            "chat_new" => {
                let p = ark_interact::held::origin_project(
                    &self.engine,
                    &s("origin"),
                    &s("origin_project"),
                    &s("name"),
                )?;
                let conv = self.open_sibling(None, &p.id)?;
                {
                    let mut ix = conv.ix.lock().await;
                    let model = s("model");
                    if !model.is_empty() {
                        ix.switch_model(&model)?;
                    }
                    conv.note(&ix);
                }
                let session = conv.session();
                self.addressed
                    .lock()
                    .unwrap()
                    .insert(session.clone(), conv.clone());
                Ok(json!({
                    "session": session,
                    "project": { "id": p.id, "name": p.name },
                    "summary": self.summary_of(&conv),
                }))
            }
            "chat_send" => {
                let text = s("text");
                anyhow::ensure!(!text.trim().is_empty(), "nothing to send");
                let ask = params["ask"].as_bool().unwrap_or(true);
                let _quiet = self.switching.lock().await;
                let conv = self.chat_conv(&session, &s("model"), true).await?;
                // The chat's model, also on a conversation that was open already (the person may have
                // changed it in this computer's own window).
                let model = s("model");
                if !model.is_empty() && !conv.is_running() {
                    let mut ix = conv.ix.lock().await;
                    ix.switch_model(&model)?;
                    conv.note(&ix);
                }
                if params.get("execution_mode").is_some() {
                    let expected: ark_interact::ark_sdk::prelude::ark_core::ExecutionMode =
                        serde_json::from_value(params["execution_mode"].clone())?;
                    let revision = params["execution_mode_revision"].as_u64().unwrap_or(0);
                    let ix = conv.ix.lock().await;
                    let summary = ix.summary();
                    anyhow::ensure!(
                        summary.execution_mode == expected
                            && summary.execution_mode_revision == revision,
                        "the execution mode changed; review it and send again"
                    );
                }
                self.start_turn_on(conv, text, ask)?;
                Ok(json!({ "started": true }))
            }
            "chat_steer" | "chat_cancel" => {
                let conv = self
                    .conv_of(&session)
                    .ok_or_else(|| anyhow::anyhow!("no turn is running"))?;
                let running = conv.running.lock().unwrap();
                let r = running
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("no turn is running"))?;
                if method == "chat_steer" {
                    r.ctl.steer.push(s("text"));
                    Ok(json!({ "queued": true }))
                } else {
                    r.ctl.cancel.cancel();
                    Ok(json!({}))
                }
            }
            // Everything a window needs to show the chat, up to `seq`: later events follow.
            "chat_read" => {
                let conv = self.chat_conv(&session, &s("model"), false).await?;
                let before = params["before"].as_u64().map(|b| b as u32);
                let limit = params["limit"].as_u64().unwrap_or(30) as usize;
                let transcript = ark_interact::interaction::transcript(
                    &self.engine,
                    &self.labels,
                    &session,
                    before,
                    limit,
                )?;
                let running = conv
                    .running
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|r| r.input.clone());
                let (seq, live) = self.feed.position(&session);
                Ok(json!({
                    "summary": self.summary_of(&conv),
                    "transcript": transcript,
                    "running": running,
                    "live": live,
                    "seq": seq,
                    "approvals": self.questions_of(&session),
                }))
            }
            "chat_approve" => {
                let decision: ApprovalDecision =
                    serde_json::from_value(params["decision"].clone())?;
                let id = params["id"].as_u64().unwrap_or(0);
                let reply = {
                    let mut approvals = self.approvals.lock().unwrap();
                    match approvals.get(&id) {
                        Some((q, _)) if q["session"] == json!(session) => {
                            approvals.remove(&id).map(|(_, r)| r)
                        }
                        _ => None,
                    }
                };
                let reply =
                    reply.ok_or_else(|| anyhow::anyhow!("that approval is no longer pending"))?;
                let _ = reply.send(decision);
                self.emit(json!({ "kind": "approval_closed", "id": id }));
                Ok(json!({}))
            }
            "chat_model" => {
                let conv = self.chat_conv(&session, &s("model"), false).await?;
                anyhow::ensure!(
                    !conv.is_running(),
                    "the agent is working; wait for the turn to finish or stop it"
                );
                let mut ix = conv.ix.lock().await;
                ix.switch_model(&s("spec"))?;
                conv.note(&ix);
                drop(ix);
                Ok(self.summary_of(&conv))
            }
            "chat_execution_mode" => {
                let conv = self.chat_conv(&session, &s("model"), false).await?;
                let mode = serde_json::from_value(params["mode"].clone())?;
                let revision = params["revision"].as_u64().unwrap_or(0);
                let mut ix = conv.ix.lock().await;
                ix.set_execution_mode(revision, mode)?;
                conv.note(&ix);
                Ok(self.summary_of(&conv))
            }
            "chat_rename" => {
                let conv = self.chat_conv(&session, &s("model"), false).await?;
                anyhow::ensure!(
                    !conv.is_running(),
                    "this chat is working; rename it when it finishes"
                );
                let mut ix = conv.ix.lock().await;
                ix.rename_session(None, &s("title"))?;
                conv.note(&ix);
                drop(ix);
                Ok(self.summary_of(&conv))
            }
            "chat_context" => {
                let conv = self.chat_conv(&session, &s("model"), false).await?;
                Ok(serde_json::to_value(self.context_of(&conv).await?)?)
            }
            // Who can answer here: the models this computer has, as `models` lists them.
            "chat_models" => Ok(serde_json::to_value(
                ark_interact::ark_sdk::prelude::ark_model::catalog().await,
            )?),
            "chat_worker_offers" => Ok(serde_json::to_value(self.worker_offers())?),
            "chat_worker_setup" => self.open_worker_setup(&session, params),
            // A program's install or sign-in, in a terminal the other runtime shows and the person
            // answers. Only the steps the catalog names run, never a command sent from there.
            "chat_setup" => {
                use ark_interact::ark_sdk::prelude::ark_model::cli::Cli;
                let program = Cli::of(&s("program"))
                    .ok_or_else(|| anyhow::anyhow!("no program `{}` here", s("program")))?;
                let argv: Vec<String> = match s("step").as_str() {
                    "install" => vec!["sh".into(), "-c".into(), program.install_line().into()],
                    "sign_in" => {
                        let bin = program.locate().ok_or_else(|| {
                            anyhow::anyhow!("{} is not installed here", program.label())
                        })?;
                        std::iter::once(bin.display().to_string())
                            .chain(program.sign_in_args().iter().map(|a| a.to_string()))
                            .collect()
                    }
                    other => anyhow::bail!("no step `{other}`"),
                };
                let home = ark_os::dirs::home().unwrap_or_else(std::env::temp_dir);
                let size = |k: &str, d: u64| params[k].as_u64().unwrap_or(d).clamp(2, 1000) as u16;
                let record = self
                    .engine
                    .store
                    .get_session(&session)?
                    .ok_or_else(|| anyhow::anyhow!("there is no chat `{session}` here"))?;
                let reference = ResourceRef {
                    location: ResourceLocation {
                        runtime: ark_interact::held::runtime_id(&self.engine),
                        project: record["project"].as_str().unwrap_or_default().to_string(),
                        session: session.clone(),
                    },
                    kind: ResourceKind::Terminal,
                    id: String::new(),
                    authority: ResourceAuthority::Chat,
                };
                let opened = self.terminals.open_program(
                    &self.feed,
                    crate::terminal::Program {
                        argv: &argv,
                        clear_env: false,
                        env: &[],
                        cwd: home,
                        cols: size("cols", 80),
                        rows: size("rows", 24),
                        reference: reference.clone(),
                    },
                );
                let opened = opened?;
                self.setup_terms
                    .lock()
                    .unwrap()
                    .insert(opened.session.clone(), session.clone());
                let mut reference = reference;
                reference.id = opened.session.clone();
                let mut value = serde_json::to_value(opened)?;
                value["ref"] = serde_json::to_value(reference)?;
                Ok(value)
            }
            // A key the person agreed to send from their other computer: saved here as
            // `api_key_save` saves one (checked with the provider first).
            "chat_key" => {
                // The person removed this computer from the other: the keys it sent go.
                // Each only while it is still the key that was sent: one saved here since stays.
                if let Some(sent) = params["forget"].as_object() {
                    let keys = self
                        .api_keys()
                        .ok_or_else(|| anyhow::anyhow!("this computer keeps no keys"))?;
                    for (provider, hash) in sent {
                        let same = keys.saved_key(provider).is_some_and(|k| {
                            use sha2::Digest;
                            Some(
                                format!("{:x}", sha2::Sha256::digest(k.trim().as_bytes())).as_str(),
                            ) == hash.as_str()
                        });
                        if same {
                            keys.forget(provider)?;
                        }
                    }
                    return Ok(json!({ "saved": keys.saved() }));
                }
                let keys = self.api_keys().ok_or_else(|| {
                    anyhow::anyhow!("this computer keeps no keys; set the provider's environment variable there")
                })?;
                keys.save(&s("provider"), &s("key")).await?;
                Ok(json!({ "saved": keys.saved() }))
            }
            "chat_trace" => {
                let conv = self.chat_conv(&session, &s("model"), false).await?;
                let index = params["index"].as_u64().map(|i| i as u32);
                Ok(serde_json::to_value(self.trace_of(&conv, index).await?)?)
            }
            other => anyhow::bail!("unknown method `{other}`"),
        }
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

    fn find_artifact_in(
        &self,
        pid: &ProjectId,
        id: &str,
    ) -> anyhow::Result<ark_sdk::prelude::Artifact> {
        let store = &self.engine.store;
        if let Some(a) = store.resolve_artifact(pid, id)? {
            return Ok(a);
        }
        for p in store.list_projects()? {
            if p.parent.as_ref() == Some(pid) {
                if let Some(a) = store.resolve_artifact(&p.id, id)? {
                    return Ok(a);
                }
            }
        }
        anyhow::bail!("artifact not found")
    }

    fn artifact_in(&self, pid: &ProjectId, id: &str, params: &Value) -> anyhow::Result<Value> {
        let art = self.find_artifact_in(pid, id)?;
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

    fn start_turn(self: &Arc<Self>, input: String, ask: bool) -> anyhow::Result<()> {
        self.start_turn_on(self.cur(), input, ask)
    }

    fn start_turn_on(
        self: &Arc<Self>,
        conv: Arc<Conv>,
        input: String,
        ask: bool,
    ) -> anyhow::Result<()> {
        let ctl = TurnControl {
            ask,
            ..TurnControl::new()
        };
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
        if let Some(focus) = conv.focus.lock().unwrap().take() {
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
            Err(_) => (false, false, end.error_line(), String::new(), 0),
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
        // A chat in the background, or held here for another runtime, ends with its turn; its
        // session is stored and opens again when called.
        if !current {
            self.background
                .lock()
                .unwrap()
                .retain(|c| !Arc::ptr_eq(c, &conv));
            self.addressed
                .lock()
                .unwrap()
                .retain(|_, c| !Arc::ptr_eq(c, &conv));
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
            "out_of_credit": end.out_of_credit(),
            "answer": answer,
            "steps": steps,
            "footer": end.footer,
            "index": end.index,
            // It failed and is stored: the person can continue it (ADR 0048).
            "resume": error.is_some() && end.index.is_some(),
            "elapsed_ms": end.elapsed_ms,
            "undelivered": end.undelivered,
            "summary": current.then(|| self.summary()),
        }));
        self.emit_working();
    }
}

/// Tell windows what project processes do (`{kind: "proc", event}`): started, output, ready
/// (with the page to open), ended.
async fn forward_processes(
    host: std::sync::Weak<Host>,
    mut rx: broadcast::Receiver<ark_sdk::prelude::ark_proc::Event>,
) {
    loop {
        match rx.recv().await {
            Ok(ev) => {
                let Some(h) = host.upgrade() else { break };
                let Some(id) = proc_id(&ev) else { continue };
                let root = proc_project(&ev).map(str::to_string).or_else(|| {
                    h.processes
                        .as_ref()
                        .and_then(|processes| processes.get(id))
                        .map(|process| process.project)
                });
                let Some(root) = root else { continue };
                let project = h.engine.store.list_projects().ok().and_then(|projects| {
                    projects.into_iter().find(|p| {
                        h.engine
                            .workspace_dir(&p.id)
                            .is_ok_and(|dir| dir == std::path::Path::new(&root))
                    })
                });
                let Some(project) = project else { continue };
                for mut reference in
                    h.resource_subscribers
                        .aliases(ResourceKind::Process, project.id.as_str(), id)
                {
                    reference.id = id.to_string();
                    h.emit(json!({
                        "kind": "proc",
                        "session": reference.location.session,
                        "ref": reference,
                        "event": ev,
                        "here": true,
                    }));
                }
            }
            // A window that missed output reads it again (`proc_output`).
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

fn proc_project(ev: &ark_sdk::prelude::ark_proc::Event) -> Option<&str> {
    use ark_sdk::prelude::ark_proc::Event;
    match ev {
        Event::Started { process } | Event::Changed { process, .. } => Some(&process.project),
        Event::Output { .. } => None,
    }
}

fn proc_id(ev: &ark_sdk::prelude::ark_proc::Event) -> Option<&str> {
    use ark_sdk::prelude::ark_proc::Event;
    match ev {
        Event::Started { process } | Event::Changed { process, .. } => Some(&process.id),
        Event::Output { id, .. } => Some(id),
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

async fn forward_computer_activity(
    host: std::sync::Weak<Host>,
    mut rx: tokio::sync::watch::Receiver<Option<ark_sdk::prelude::ark_computer::ComputerActivity>>,
) {
    while rx.changed().await.is_ok() {
        let Some(h) = host.upgrade() else { break };
        let activity = rx.borrow().clone();
        let line = activity
            .as_ref()
            .map(ark_interact::labels::computer_activity_line)
            .unwrap_or_default();
        h.emit(json!({ "kind": "computer_action", "activity": activity, "line": line }));
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
        let project = host.task_project(pending.request.task.as_deref());
        let shown = json!({ "request": pending.request, "line": line, "title": title, "choices": choices, "session": session, "chat": chat, "project": project, "asked_ms": crate::notify::now_ms() });
        host.approvals
            .lock()
            .unwrap()
            .insert(id, (shown.clone(), reply));
        host.notify_soon();
        host.emit(
            json!({ "kind": "approval", "id": id, "request": shown["request"], "line": line, "title": shown["title"], "choices": shown["choices"], "session": session, "chat": chat, "project": shown["project"] }),
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

#[cfg(test)]
mod dictation_tests {
    use super::*;

    #[test]
    fn chunks_stop_at_exactly_eight_minutes_without_advancing_on_rejection() {
        let mut uploads = HashMap::new();
        let id = begin_dictation_upload(&mut uploads, "alice".into()).unwrap();
        let upload = uploads.get_mut(&id).unwrap();
        upload.bytes = 8 * 60 * 16_000 * 2 - 2;
        assert_eq!(
            write_dictation_chunk(upload, "alice", 0, &[0, 0]).unwrap(),
            1
        );
        assert_eq!(upload.bytes, 15_360_000);
        let file_len = upload.file.as_file().metadata().unwrap().len();
        assert!(write_dictation_chunk(upload, "alice", 1, &[0, 0]).is_err());
        assert_eq!(upload.bytes, 15_360_000);
        assert_eq!(upload.next, 1);
        assert_eq!(upload.chunks.len(), 1);
        assert_eq!(upload.file.as_file().metadata().unwrap().len(), file_len);
    }

    #[test]
    fn uploads_are_one_per_owner_and_expire() {
        let mut uploads = HashMap::new();
        let id = begin_dictation_upload(&mut uploads, "alice".into()).unwrap();
        assert!(begin_dictation_upload(&mut uploads, "alice".into()).is_err());
        begin_dictation_upload(&mut uploads, "bob".into()).unwrap();
        uploads.get_mut(&id).unwrap().touched =
            std::time::Instant::now() - std::time::Duration::from_secs(601);
        uploads.retain(|_, upload| upload.touched.elapsed() < std::time::Duration::from_secs(600));
        assert!(!uploads.contains_key(&id));
        assert_eq!(uploads.len(), 1);
    }

    #[test]
    fn chunks_enforce_owner_order_limits_and_idempotence() {
        let mut uploads = HashMap::new();
        let id = begin_dictation_upload(&mut uploads, "alice".into()).unwrap();
        let upload = uploads.get_mut(&id).unwrap();
        assert!(write_dictation_chunk(upload, "mallory", 0, b"one").is_err());
        assert!(write_dictation_chunk(upload, "alice", 1, b"one").is_err());
        assert_eq!(
            write_dictation_chunk(upload, "alice", 0, b"one").unwrap(),
            1
        );
        assert_eq!(
            write_dictation_chunk(upload, "alice", 0, b"one").unwrap(),
            1
        );
        assert!(write_dictation_chunk(upload, "alice", 0, b"two").is_err());
        assert!(write_dictation_chunk(upload, "alice", 1, &vec![0; DICTATION_CHUNK + 1]).is_err());
    }
}

#[cfg(test)]
mod whatsapp_tests {
    use super::without;

    #[test]
    fn an_error_never_echoes_a_secret_back() {
        let said =
            "Meta refused the access token: Invalid OAuth access token EAAB123 for secret s3cr3t";
        let out = without(said, &["EAAB123".into(), "s3cr3t".into(), String::new()]);
        assert_eq!(
            out,
            "Meta refused the access token: Invalid OAuth access token ... for secret ..."
        );
    }
}
