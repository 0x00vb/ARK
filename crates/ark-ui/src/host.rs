//! The workspace host: one [`Interaction`] behind a small request/event protocol. Transports
//! (the desktop window, the local browser server, tests) call [`Host::call`] and subscribe to
//! [`Host::subscribe`]; none of them holds agent logic.
//!
//! A turn runs in its own task and holds the interaction for its duration. Requests that only
//! read the store (working state, artifacts, invocations, stored traces) are served from the
//! shared engine while it runs; the rest answer from a snapshot taken after the last change, or
//! say the agent is busy.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ark_interact::activity::describe;
use ark_interact::ark_orchestrate::{AgentSpec, Orchestrator, TaskSpec};
use ark_interact::inspect::{
    ArtifactSummary, ContextSource, ContextView, KeptOut, StateView, UsageView,
};
use ark_interact::interaction::load_trace;
use ark_interact::orchestration::event_line;
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
    /// Events since the running turn started; `None` between turns.
    live: Option<Vec<Value>>,
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
        if kind == "turn_started" {
            st.live = Some(Vec::new());
        }
        if let Some(live) = st.live.as_mut() {
            if live.len() < LIVE_EVENTS {
                live.push(v.clone());
            }
        }
        if kind == "turn_finished" {
            st.live = None;
        }
        let _ = self.tx.send(v);
    }

    /// The last sequence number sent, and the running turn's events so far.
    pub(crate) fn position(&self) -> (u64, Vec<Value>) {
        let st = self.state.lock().unwrap();
        (st.seq, st.live.clone().unwrap_or_default())
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

/// Presentation settings sent to the page.
#[derive(Clone, Debug, Default, Serialize)]
pub struct UiInfo {
    pub accent: Option<String>,
    pub logo: Option<String>,
    pub welcome: Welcome,
    pub scripts: Vec<String>,
    pub styles: Vec<String>,
}

struct Running {
    ctl: TurnControl,
    input: String,
}

/// Cached views, refreshed whenever the interaction is free.
#[derive(Default)]
struct Snapshot {
    summary: Value,
    usage: Option<UsageView>,
    context: Option<ContextView>,
    kept_out: KeptOut,
    project: Option<Project>,
    session: String,
}

pub struct Host {
    ix: tokio::sync::Mutex<Interaction>,
    engine: Arc<Engine>,
    labels: Labels,
    feed: Arc<Feed>,
    running: Mutex<Option<Running>>,
    approvals: Mutex<HashMap<u64, (Value, oneshot::Sender<ApprovalDecision>)>>,
    next_id: AtomicU64,
    focus: Mutex<Option<Option<Focus>>>,
    snapshot: Mutex<Snapshot>,
    handlers: HashMap<String, Handler>,
    info: UiInfo,
    commands: Value,
    /// The current project's orchestrator, and the task forwarding its events.
    orch: Mutex<Option<(Orchestrator, tokio::task::JoinHandle<()>)>>,
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
            engine: ix.ark().shared_engine(),
            labels: ix.labels().clone(),
            commands: serde_json::to_value(ix.commands()).unwrap_or_default(),
            ix: tokio::sync::Mutex::new(ix),
            feed: Feed::new(),
            running: Mutex::new(None),
            approvals: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            focus: Mutex::new(None),
            snapshot: Mutex::new(Snapshot::default()),
            handlers,
            info,
        });
        host.refresh().await;
        tokio::spawn(forward_approvals(host.clone(), approvals));
        host
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

    pub fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }

    /// Rebuild the cached views from the interaction (call while it is free).
    async fn refresh(&self) {
        let ix = self.ix.lock().await;
        self.refresh_with(&ix).await;
    }

    async fn refresh_with(&self, ix: &Interaction) {
        self.follow_orchestrator(ix.orchestrator());
        let context = ix.context_view().await.ok();
        let kept_out = ix.kept_out().unwrap_or_default();
        let mut s = self.snapshot.lock().unwrap();
        s.summary = serde_json::to_value(ix.summary()).unwrap_or_default();
        s.usage = Some(ix.usage_view());
        s.context = context;
        s.kept_out = kept_out;
        s.project = Some(ix.project().clone());
        s.session = ix.session().id().as_str().to_string();
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
        self.snapshot.lock().unwrap().session.clone()
    }

    fn summary(&self) -> Value {
        let mut v = self.snapshot.lock().unwrap().summary.clone();
        v["running"] = json!(self.is_running());
        v
    }

    /// Lock the interaction for a request that changes it; refuse while a turn runs.
    async fn idle(&self) -> anyhow::Result<tokio::sync::MutexGuard<'_, Interaction>> {
        if self.is_running() {
            anyhow::bail!("the agent is working; wait for the turn to finish or stop it");
        }
        Ok(self.ix.lock().await)
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
                    .map(|(id, (req, _))| json!({ "id": id, "request": req["request"], "line": req["line"] }))
                    .collect();
                let running = self
                    .running
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|r| r.input.clone());
                // Everything up to `seq` is in this answer: stored turns through `summary`,
                // the running turn through `live`. A window ignores events at or below it.
                let (seq, live) = self.feed.position();
                Ok(json!({
                    "summary": self.summary(),
                    "ui": self.info,
                    "commands": self.commands,
                    "running": running,
                    "live": live,
                    "seq": seq,
                    "approvals": pending,
                    "version": env!("CARGO_PKG_VERSION"),
                }))
            }
            "summary" => Ok(self.summary()),
            "send" => {
                let text = s("text");
                if text.trim().is_empty() {
                    anyhow::bail!("nothing to send");
                }
                self.start_turn(text)?;
                Ok(json!({ "started": true }))
            }
            "steer" => {
                let text = s("text");
                match self.running.lock().unwrap().as_ref() {
                    Some(r) => {
                        r.ctl.steer.push(text);
                        Ok(json!({ "queued": true }))
                    }
                    None => anyhow::bail!("no turn is running"),
                }
            }
            "cancel" => {
                if let Some(r) = self.running.lock().unwrap().as_ref() {
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
            "sessions" => {
                let ix = self.idle().await?;
                let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
                Ok(serde_json::to_value(ix.sessions(all)?)?)
            }
            "projects" => Ok(serde_json::to_value(self.idle().await?.projects()?)?),
            "models" => Ok(serde_json::to_value(
                ark_interact::ark_sdk::prelude::ark_model::catalog().await,
            )?),
            "new_session" => self.change(|ix| ix.new_session()).await,
            "open_session" => {
                let id = s("id");
                self.change(move |ix| {
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
                self.change(move |ix| {
                    let target = (!id.is_empty()).then_some(id.as_str());
                    ix.rename_session(target, &title).map(|_| ())
                })
                .await
            }
            "switch_project" => {
                let name = s("name");
                self.change(move |ix| ix.switch_project(&name).map(|_| ()))
                    .await
            }
            "model" => {
                let spec = s("spec");
                self.change(move |ix| ix.switch_model(&spec).map(|_| ()))
                    .await
            }
            "transcript" => {
                let ix = self.idle().await?;
                let before = n("before").map(|b| b as u32);
                let limit = n("limit").unwrap_or(30) as usize;
                Ok(serde_json::to_value(ix.transcript(before, limit)?)?)
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
            "spawn_agent" => {
                let spec: AgentSpec = serde_json::from_value(params["spec"].clone())?;
                Ok(serde_json::to_value(
                    self.orchestrator()?.spawn(spec).await?,
                )?)
            }
            "submit_task" => {
                let spec: TaskSpec = serde_json::from_value(params["task"].clone())?;
                if spec.objective.trim().is_empty() {
                    anyhow::bail!("a task needs an objective");
                }
                let id = self.orchestrator()?.submit(&s("agent"), spec)?;
                Ok(json!({ "task": id }))
            }
            "cancel_task" => {
                self.orchestrator()?.cancel(&s("task"))?;
                Ok(json!({}))
            }
            "retry_task" => {
                self.orchestrator()?.retry(&s("task"))?;
                Ok(json!({}))
            }
            "suspend_agent" => {
                self.orchestrator()?.suspend(&s("agent"))?;
                Ok(json!({}))
            }
            "resume_agent" => {
                self.orchestrator()?.resume(&s("agent"))?;
                Ok(json!({}))
            }
            "terminate_agent" => {
                self.orchestrator()?.terminate(&s("agent")).await?;
                Ok(json!({}))
            }
            // A note from the person: into the agent's running task, else its next one.
            "message_agent" => {
                let delivered = self
                    .orchestrator()?
                    .message("user", &s("agent"), &s("text"))?;
                Ok(json!({ "delivered": delivered }))
            }
            // The app's actions, which an agent's `tools` patterns choose from.
            "tools" => {
                let mut actions = self.engine.registry.actions();
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
        self.refresh_with(&ix).await;
        drop(ix);
        let summary = self.summary();
        self.emit(json!({ "kind": "summary", "summary": summary }));
        Ok(summary)
    }

    async fn trace(&self, index: Option<u32>) -> anyhow::Result<Option<ark_interact::Trace>> {
        if let Some(i) = index {
            return Ok(load_trace(&self.engine, &self.session_id(), i).map(|r| interpret(&r)));
        }
        if let Some(r) = self.running.lock().unwrap().as_ref() {
            let rec = r.ctl.recorder.lock().unwrap().snapshot();
            return Ok(Some(interpret(&rec)));
        }
        Ok(self.ix.lock().await.trace(None))
    }

    async fn context(&self) -> anyhow::Result<ContextView> {
        if let Some(r) = self.running.lock().unwrap().as_ref() {
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
        let ctl = TurnControl::new();
        {
            let mut running = self.running.lock().unwrap();
            if running.is_some() {
                anyhow::bail!("the agent is already working");
            }
            *running = Some(Running {
                ctl: ctl.clone(),
                input: input.clone(),
            });
        }
        let host = self.clone();
        tokio::spawn(async move { host.run_turn(input, ctl).await });
        Ok(())
    }

    async fn run_turn(self: Arc<Self>, input: String, ctl: TurnControl) {
        self.emit(json!({ "kind": "turn_started", "input": input }));
        let mut ix = self.ix.lock().await;
        if let Some(focus) = self.focus.lock().unwrap().take() {
            ix.set_focus(focus);
        }
        let feed = self.feed.clone();
        let end = ix
            .run_turn(&input, &ctl, |ev, a| {
                // Text arrives in small pieces; everything else carries its description.
                let v = match ev {
                    HarnessEvent::TextDelta { text } => json!({ "kind": "text", "text": text }),
                    _ => json!({ "kind": "event", "event": ev, "activity": a }),
                };
                feed.send(v);
            })
            .await;
        let (ok, cancelled, error, answer, steps) = match &end.result {
            Ok(o) => (true, o.cancelled, None, o.answer.clone(), o.steps),
            Err(e) => (false, false, Some(e.to_string()), String::new(), 0),
        };
        self.refresh_with(&ix).await;
        drop(ix);
        *self.running.lock().unwrap() = None;
        // No question outlives its turn; close them before saying the turn ended. Questions
        // from orchestrated agents belong to their tasks, not to this turn.
        let open: Vec<u64> = {
            let mut approvals = self.approvals.lock().unwrap();
            let ids: Vec<u64> = approvals
                .iter()
                .filter(|(_, (shown, _))| shown["request"]["agent"].is_null())
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
            "ok": ok,
            "cancelled": cancelled,
            "error": error,
            "answer": answer,
            "steps": steps,
            "footer": end.footer,
            "index": end.index,
            "elapsed_ms": end.elapsed_ms,
            "undelivered": end.undelivered,
            "summary": self.summary(),
        }));
    }
}

/// Move each approval question to the windows, and each answer back.
async fn forward_approvals(host: Arc<Host>, mut rx: mpsc::UnboundedReceiver<PendingApproval>) {
    while let Some(mut pending) = rx.recv().await {
        let id = host.next_id.fetch_add(1, Ordering::SeqCst);
        let line = host.labels.approval_line(&pending.request);
        let (reply, answer) = oneshot::channel();
        let shown = json!({ "request": pending.request, "line": line });
        host.approvals
            .lock()
            .unwrap()
            .insert(id, (shown.clone(), reply));
        host.emit(
            json!({ "kind": "approval", "id": id, "request": shown["request"], "line": line }),
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
