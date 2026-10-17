//! [`App`] describes an ARK application once: its name, agent, commands, labels and status
//! sections. [`Interaction`] is one front-end's live connection to it: the project, the
//! session, the approval gate, and the turns run so far. The terminal shell and the GUI both
//! open an `App` into an `Interaction` and drive it through the same methods, so a session
//! operation, a command or a trace means the same thing in both.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ark_sdk::prelude::ark_core::Focus;
use ark_sdk::prelude::ark_runtime::{CompactReport, STATE_DIGEST_ITEMS};
use ark_sdk::prelude::*;

use crate::activity::{self, Activity, ActivityKind};
use crate::command::{parse, Command, CommandInfo, Reply};
use crate::inspect::{
    self, ContextSource, ContextView, KeptOut, Pricing, ProjectSummary, SessionSummary, StateView,
    TranscriptTurn, UsageView,
};
use crate::labels::{ActivityLabels, Labels, ToolLabeler};
use crate::report::{Block, Report};
use crate::trace::{self, SharedRecorder, Trace, TurnRecord, TurnRecorder};

pub type ProjectFn = Box<dyn Fn(&Ark) -> Result<Project, ArkError> + Send + Sync>;
/// Contributes a titled section to `/status`. An empty list hides it.
pub type StatusFn = Arc<dyn Fn(&Ark, &Project) -> Vec<Block> + Send + Sync>;
/// Called after the interaction moves to another project.
pub type ProjectHook = Arc<dyn Fn(&Project) + Send + Sync>;
/// Builds a project's orchestrator. The gate asks the front-end's approver; its grants last
/// as long as the interaction.
pub type OrchestrateFn = Arc<
    dyn Fn(&Ark, &Project, Arc<ApprovalGate>) -> anyhow::Result<ark_orchestrate::Orchestrator>
        + Send
        + Sync,
>;

/// An ARK application, described once and opened by any front-end.
pub struct App {
    name: String,
    tagline: Option<String>,
    help: Option<String>,
    ark: Option<Ark>,
    ark_builder: Option<ArkBuilder>,
    project: Option<ProjectFn>,
    commands: Vec<Arc<dyn Command>>,
    labelers: Vec<Arc<dyn ToolLabeler>>,
    status: Vec<(String, StatusFn)>,
    on_project: Option<ProjectHook>,
    pricing: Option<Pricing>,
    orchestrator: Option<ark_orchestrate::Orchestrator>,
    orchestrate: Option<OrchestrateFn>,
    sign_in: Option<crate::capabilities::Opener>,
}

/// How to open an [`App`]: the generic choices a person makes at launch.
#[derive(Clone, Debug, Default)]
pub struct Start {
    /// `<model>` or `<provider>:<model>`.
    pub model: Option<String>,
    /// A project name or id (created if missing).
    pub project: Option<String>,
    /// A session id or prefix to continue.
    pub session: Option<String>,
    /// Continue the most recent session.
    pub resume: bool,
}

impl App {
    pub fn new(name: impl Into<String>) -> Self {
        App {
            name: name.into(),
            tagline: None,
            help: None,
            ark: None,
            ark_builder: None,
            project: None,
            commands: Vec::new(),
            labelers: Vec::new(),
            status: Vec::new(),
            on_project: None,
            pricing: None,
            orchestrator: None,
            orchestrate: None,
            sign_in: None,
        }
    }
    /// How a sign-in page is shown when an agent asks to connect a capability. Default: the
    /// person's browser on this machine.
    pub fn sign_in(mut self, open: crate::capabilities::Opener) -> Self {
        self.sign_in = Some(open);
        self
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    /// One line describing the app.
    pub fn tagline(mut self, t: impl Into<String>) -> Self {
        self.tagline = Some(t.into());
        self
    }
    /// Text at the top of `/help`.
    pub fn help(mut self, intro: impl Into<String>) -> Self {
        self.help = Some(intro.into());
        self
    }
    /// The fully wired agent (tools, profile, model, store).
    pub fn ark(mut self, ark: Ark) -> Self {
        self.ark = Some(ark);
        self
    }
    /// Build the agent at open time instead (after `--model` is known).
    pub fn ark_builder(mut self, b: ArkBuilder) -> Self {
        self.ark_builder = Some(b);
        self
    }
    /// Shorthand: set the agent profile on the internal builder.
    pub fn agent(mut self, profile: AgentProfile) -> Self {
        self.ark_builder = Some(self.ark_builder.take().unwrap_or_default().profile(profile));
        self
    }
    /// Shorthand: register a tool on the internal builder.
    pub fn tool(mut self, adapter: Arc<dyn Adapter>) -> Self {
        self.ark_builder = Some(self.ark_builder.take().unwrap_or_default().tool(adapter));
        self
    }
    /// Shorthand: where the store lives.
    pub fn store_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.ark_builder = Some(self.ark_builder.take().unwrap_or_default().store_root(root));
        self
    }
    /// Which project to open in (default: one named after the app).
    pub fn project(
        mut self,
        f: impl Fn(&Ark) -> Result<Project, ArkError> + Send + Sync + 'static,
    ) -> Self {
        self.project = Some(Box::new(f));
        self
    }
    /// Add a domain command.
    pub fn command(mut self, c: Arc<dyn Command>) -> Self {
        self.commands.push(c);
        self
    }
    /// Custom wording for your tools.
    pub fn labeler(mut self, l: Arc<dyn ToolLabeler>) -> Self {
        self.labelers.push(l);
        self
    }
    /// Shorthand for `labeler(Arc::new(labels))`.
    pub fn labels(self, labels: ActivityLabels) -> Self {
        self.labeler(Arc::new(labels))
    }
    /// Add a titled section to `/status`.
    pub fn status_section(
        mut self,
        title: impl Into<String>,
        f: impl Fn(&Ark, &Project) -> Vec<Block> + Send + Sync + 'static,
    ) -> Self {
        self.status.push((title.into(), Arc::new(f)));
        self
    }
    /// Run after every project switch (e.g. to remember the current project).
    pub fn on_project_change(mut self, f: impl Fn(&Project) + Send + Sync + 'static) -> Self {
        self.on_project = Some(Arc::new(f));
        self
    }
    /// The app runs several agents: `/agents` shows their graph and each task's trace, and a
    /// window can follow it live.
    pub fn orchestrator(mut self, o: ark_orchestrate::Orchestrator) -> Self {
        self.orchestrator = Some(o);
        self
    }
    /// The app runs several agents in every project: `f` builds a project's orchestrator the
    /// first time the interaction works there, with a gate that asks the front-end. Its
    /// agents' approvals then reach the person like the conversation's own.
    pub fn orchestrate(
        mut self,
        f: impl Fn(&Ark, &Project, Arc<ApprovalGate>) -> anyhow::Result<ark_orchestrate::Orchestrator>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.orchestrate = Some(Arc::new(f));
        self
    }
    /// Prices for the usage view. Without them (and without `ARK_PRICE`) no cost is shown.
    pub fn pricing(mut self, p: Pricing) -> Self {
        self.pricing = Some(p);
        self
    }

    /// Open the app: build the agent, pick the project and session. `approver` answers the
    /// approval gate (a front-end passes a [`crate::ChannelApprover`]).
    pub fn open(mut self, start: &Start, approver: Arc<dyn Approver>) -> anyhow::Result<Opened> {
        let mut ark = match (self.ark.take(), self.ark_builder.take()) {
            (Some(a), _) => a,
            (None, Some(b)) => b.build()?,
            (None, None) => Ark::builder().build()?,
        };
        if let Some(spec) = &start.model {
            let (provider, model) = model_spec(spec, &ark);
            ark.set_router(ModelRouter::single(ark_model::build(
                &provider,
                Some(model),
            )?));
        }
        let project = match &start.project {
            Some(p) => ark.find_or_create_project(p, "")?,
            None => match &self.project {
                Some(f) => f(&ark)?,
                None => ark.find_or_create_project(&self.name.to_lowercase(), "")?,
            },
        };
        let mut resumed = false;
        let session = if let Some(id) = &start.session {
            resumed = true;
            ark.open_session(id)?
                .ok_or_else(|| anyhow::anyhow!("no session matches `{id}`"))?
        } else if start.resume {
            match ark.latest_session(&project.id)? {
                Some(s) => {
                    resumed = true;
                    s
                }
                None => ark.start_session(&project.id)?,
            }
        } else {
            ark.start_session(&project.id)?
        };
        // A resumed session may live in another project.
        let project = if session.project() != &project.id {
            ark.engine()
                .store
                .get_project(session.project())?
                .unwrap_or(project)
        } else {
            project
        };
        // A yes to "ARK needs Mail" connects it before the turn goes on; a yes to "Install
        // Node.js" installs it.
        let approver = crate::installs::wrap(approver, &ark);
        let approver = crate::capabilities::wrap_computer(approver, &ark);
        let approver: Arc<dyn Approver> = match ark.connections() {
            Some(c) => {
                let mut connector = crate::capabilities::Connector::new(approver, c.clone());
                if let Some(open) = self.sign_in.take() {
                    connector = connector.opener(open);
                }
                Arc::new(connector)
            }
            None => approver,
        };
        let mut commands = self.commands;
        commands.extend(crate::builtins::all());
        if self.orchestrator.is_some() || self.orchestrate.is_some() {
            commands.extend(crate::builtins::agents());
        }
        let orchs = self.orchestrate.map(|build| {
            Arc::new(Orchestrators {
                build,
                approver: approver.clone(),
                built: std::sync::Mutex::new(HashMap::new()),
                events: tokio::sync::broadcast::channel(1024).0,
            })
        });
        let mut labelers = self.labelers;
        // A schedule's change names the schedule it is about.
        labelers.push(Arc::new(crate::labels::ScheduleNames(ark.clone())));
        let mut ix = Interaction {
            info: AppInfo {
                name: self.name,
                tagline: self.tagline,
                help: self.help,
            },
            ark,
            project,
            session,
            gate: Arc::new(ApprovalGate::new(approver)),
            commands,
            labels: Labels::new(labelers),
            status: self.status,
            on_project: self.on_project,
            pricing: self.pricing.or_else(Pricing::from_env),
            keys: Vec::new(),
            history: Vec::new(),
            last_context: None,
            detail: false,
            focus: None,
            names: HashMap::new(),
            present: false,
            orchestrator: self.orchestrator,
            orchs,
            sign_in: self.sign_in,
            links: Arc::new(crate::links::Links::default()),
            zone: Arc::new(std::sync::Mutex::new(None)),
            waiting: Vec::new(),
        };
        ix.refresh_names();
        ix.follow_project()?;
        // Projects with scheduled work get their orchestrator now, so it runs with no window
        // or chat open there.
        if let Some(o) = &ix.orchs {
            o.wake_scheduled(&ix.ark);
        }
        Ok(Opened {
            interaction: ix,
            resumed,
        })
    }
}

/// The projects a person works in (not orchestrated agents' child projects), newest first.
pub(crate) fn top_projects(engine: &Engine) -> anyhow::Result<Vec<Project>> {
    Ok(engine
        .store
        .list_projects()
        .map_err(ark_runtime::AgentError::from)?
        .into_iter()
        .filter(|p| p.parent.is_none())
        .collect())
}

/// [`Interaction::projects`] without the interaction: read from the store while a turn runs.
pub fn list_projects(engine: &Engine, current: &ProjectId) -> anyhow::Result<Vec<ProjectSummary>> {
    Ok(top_projects(engine)?
        .into_iter()
        .map(|p| ProjectSummary {
            id: p.id.as_str().to_string(),
            current: &p.id == current,
            name: p.name,
            target: p.target,
            workspace: p.workspace,
        })
        .collect())
}

/// Sessions, most recent first, of one project or all (`scope: None`); `current` is marked.
pub fn list_sessions(
    engine: &Engine,
    scope: Option<&ProjectId>,
    current: &str,
) -> anyhow::Result<Vec<SessionSummary>> {
    let names: HashMap<String, String> = engine
        .store
        .list_projects()
        .map_err(ark_runtime::AgentError::from)?
        .into_iter()
        .map(|p| (p.id.as_str().to_string(), p.name))
        .collect();
    Ok(Session::list(engine, scope)?
        .iter()
        .map(|r| {
            let pname = names.get(r.project.as_str()).cloned().unwrap_or_default();
            SessionSummary::of(r, &pname, r.id.as_str() == current)
        })
        .collect())
}

/// [`Interaction::project_sessions`] without the interaction.
pub fn project_sessions(
    engine: &Engine,
    id: &str,
    current: &str,
) -> anyhow::Result<Vec<SessionSummary>> {
    let p = top_projects(engine)?
        .into_iter()
        .find(|p| p.id.as_str() == id)
        .ok_or_else(|| anyhow::anyhow!("no project with id `{id}`"))?;
    Ok(Session::list(engine, Some(&p.id))?
        .iter()
        .map(|r| SessionSummary::of(r, &p.name, r.id.as_str() == current))
        .collect())
}

/// [`Interaction::transcript`] of any stored session, without its interaction: a window reads a
/// chat while that chat's turn runs.
pub fn transcript(
    engine: &Engine,
    labels: &Labels,
    session: &str,
    before: Option<u32>,
    limit: usize,
) -> anyhow::Result<Vec<TranscriptTurn>> {
    let s =
        Session::open(engine, session)?.ok_or_else(|| anyhow::anyhow!("no session `{session}`"))?;
    let names = engine
        .store
        .list_artifacts(s.project())
        .map(|a| inspect::artifact_names(&a))
        .unwrap_or_default();
    transcript_page(engine, labels, &names, &s, before, limit)
}

fn transcript_page(
    engine: &Engine,
    labels: &Labels,
    names: &HashMap<String, String>,
    session: &Session,
    before: Option<u32>,
    limit: usize,
) -> anyhow::Result<Vec<TranscriptTurn>> {
    let sid = session.id().as_str().to_string();
    let posts = ark_sdk::prelude::ark_runtime::session::posts(engine, &sid, 0, usize::MAX)?;
    let last = session.turn_count().checked_sub(1);
    Ok(session
        .transcript_page(engine, before, limit)?
        .iter()
        .map(|t| {
            let rec = load_trace(engine, &sid, t.index);
            // Replay in order: narration (the recorder merges a step's text into one
            // event) where it was written, steering where it was read.
            let answer = t.answer.trim();
            let activity = rec
                .as_ref()
                .map(|r| {
                    r.events
                        .iter()
                        .filter_map(|e| match &e.event {
                            HarnessEvent::TextDelta { text }
                                if !text.trim().is_empty() && text.trim() != answer =>
                            {
                                Some(Activity::new(ActivityKind::Narration, text.trim()))
                            }
                            ev => activity::describe(labels, names, ev),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut activity = activity;
            // A turn that did not finish: each call as its record says, even one the trace
            // never got to (the app stopped between the call and the trace's next save).
            if t.incomplete || t.cancelled {
                let reported: std::collections::HashSet<String> = rec
                    .iter()
                    .flat_map(|r| &r.events)
                    .filter_map(|e| match &e.event {
                        HarnessEvent::ToolResult { invocation, .. } => Some(invocation.clone()),
                        _ => None,
                    })
                    .collect();
                let calls = engine
                    .work_calls(session.project(), &session.work_of(t.index))
                    .unwrap_or_default();
                activity::settle(&mut activity, &reported, &calls, labels, names);
            }
            let mut turn = TranscriptTurn::of(t, activity, rec.is_some());
            turn.resume = t.incomplete && Some(t.index) == last;
            // Each post goes after the turn that was last when it came.
            turn.posts = posts
                .iter()
                .filter(|(_, p)| p.after.unwrap_or(0) == t.index)
                .map(|(_, p)| p.clone())
                .collect();
            turn
        })
        .collect())
}

/// The orchestrators of an app that runs agents in every project ([`App::orchestrate`]), one per
/// project, shared by all of the app's live conversations: work keeps running in a project no
/// conversation has open, and coming back finds the same orchestrator. Each has its own approval
/// gate, kept in its project, so "Always in this project" means that agent's project.
pub(crate) struct Orchestrators {
    build: OrchestrateFn,
    approver: Arc<dyn Approver>,
    built: std::sync::Mutex<HashMap<String, ark_orchestrate::Orchestrator>>,
    /// Every orchestrator's events, also of ones built later.
    events: tokio::sync::broadcast::Sender<ark_orchestrate::OrchEvent>,
}

impl Orchestrators {
    fn get(&self, ark: &Ark, project: &Project) -> anyhow::Result<ark_orchestrate::Orchestrator> {
        let mut built = self.built.lock().unwrap();
        if let Some(o) = built.get(project.id.as_str()) {
            return Ok(o.clone());
        }
        let gate = Arc::new(ApprovalGate::new(self.approver.clone()));
        gate.keep_in(ark.engine().store.clone(), project.id.as_str());
        let o = (self.build)(ark, project, gate)?;
        built.insert(project.id.as_str().to_string(), o.clone());
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(forward(o.subscribe(), self.events.clone()));
        }
        Ok(o)
    }

    /// Open the orchestrators of the projects that have schedules still running or work a
    /// stopped process left open, so that work goes on with no window.
    fn wake_scheduled(&self, ark: &Ark) {
        let store = &ark.engine().store;
        for id in ark_orchestrate::projects_with_work(store) {
            match store.get_project(&ProjectId(id.clone())) {
                Ok(Some(p)) => {
                    if let Err(e) = self.get(ark, &p) {
                        tracing::warn!(project = %id, "its work cannot run: {e:#}");
                    }
                }
                _ => tracing::warn!(project = %id, "work names a project that is gone"),
            }
        }
    }
}

/// Every orchestrator an app has open: one per project that has work (with
/// [`App::orchestrate`]), or the app's only one. Cheap to clone; a front-end keeps one to list
/// what waits for the person in any project (ADR 0036).
#[derive(Clone, Default)]
pub struct OrchestratorSet {
    all: Option<Arc<Orchestrators>>,
    one: Option<ark_orchestrate::Orchestrator>,
}

impl OrchestratorSet {
    pub fn list(&self) -> Vec<ark_orchestrate::Orchestrator> {
        match &self.all {
            Some(all) => all.built.lock().unwrap().values().cloned().collect(),
            None => self.one.iter().cloned().collect(),
        }
    }

    /// The events of every orchestrator in the set, also of ones opened after this call.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ark_orchestrate::OrchEvent> {
        match (&self.all, &self.one) {
            (Some(all), _) => all.events.subscribe(),
            (None, Some(o)) => o.subscribe(),
            (None, None) => tokio::sync::broadcast::channel(1).1,
        }
    }

    /// Answer the question a task keeps, in whichever project it waits. Errs when no task
    /// waits on that question any more.
    pub fn answer(&self, question: &str, decision: ApprovalDecision) -> anyhow::Result<()> {
        let (o, task) = self
            .list()
            .into_iter()
            .find_map(|o| {
                let t = o
                    .questions()
                    .into_iter()
                    .find(|t| t.question.as_ref().is_some_and(|q| q.id == question))?;
                Some((o, t.id))
            })
            .ok_or_else(|| anyhow::anyhow!("that question is no longer waiting"))?;
        o.answer(&task, decision)?;
        Ok(())
    }
}

async fn forward(
    mut rx: tokio::sync::broadcast::Receiver<ark_orchestrate::OrchEvent>,
    tx: tokio::sync::broadcast::Sender<ark_orchestrate::OrchEvent>,
) {
    use tokio::sync::broadcast::error::RecvError;
    loop {
        match rx.recv().await {
            Ok(e) => {
                let _ = tx.send(e);
            }
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return,
        }
    }
}

/// Opens more live conversations of the same app: a window keeps one chat working while the
/// person starts or opens another. They share the agent (store, tools, the model chosen last,
/// capabilities), the orchestrators and lasting grants; each has its own session, its own
/// answers "for this session", and its own turns.
#[derive(Clone)]
pub struct Siblings {
    info: AppInfo,
    ark: Ark,
    commands: Vec<Arc<dyn Command>>,
    labels: Labels,
    status: Vec<(String, StatusFn)>,
    on_project: Option<ProjectHook>,
    pricing: Option<Pricing>,
    keys: Vec<(String, String)>,
    present: bool,
    orchestrator: Option<ark_orchestrate::Orchestrator>,
    orchs: Option<Arc<Orchestrators>>,
    sign_in: Option<crate::capabilities::Opener>,
    links: Arc<crate::links::Links>,
    zone: Zone,
}

/// The person's IANA time zone as their client said it, shared by an app's conversations.
type Zone = Arc<std::sync::Mutex<Option<String>>>;

impl Siblings {
    /// The person's time zone, for every conversation of the app ([`Interaction::set_zone`]).
    pub fn set_zone(&self, tz: Option<&str>) {
        let known = tz.filter(|t| {
            ark_orchestrate::When::daily_at("09:00")
                .in_zone(*t)
                .normalized()
                .is_ok()
        });
        *self.zone.lock().unwrap() = known.map(str::to_string);
    }

    /// Tell the app the conversation windows show is now in `project` (what
    /// [`App::on_project_change`] gets), when a front-end brings forward a conversation that is
    /// already open there.
    pub fn project_changed(&self, project: &Project) {
        if let Some(f) = &self.on_project {
            f(project);
        }
    }

    /// Another live conversation: in session `session` (an id), or a new session in `project`.
    /// `approver` gets its questions (each conversation's own, so a front-end knows which
    /// conversation asks).
    pub fn open(
        &self,
        session: Option<&str>,
        project: &ProjectId,
        approver: Arc<dyn Approver>,
    ) -> anyhow::Result<Interaction> {
        let mut ark = self.ark.clone();
        ark.follow_current_model();
        let session = match session {
            Some(id) => ark
                .open_session(id)?
                .ok_or_else(|| anyhow::anyhow!("no session matches `{id}`"))?,
            None => ark.start_session(project)?,
        };
        let project = ark
            .engine()
            .store
            .get_project(session.project())?
            .ok_or_else(|| anyhow::anyhow!("the session's project is gone"))?;
        let approver = crate::installs::wrap(approver, &ark);
        let approver = crate::capabilities::wrap_computer(approver, &ark);
        let approver: Arc<dyn Approver> = match ark.connections() {
            Some(c) => {
                let mut connector = crate::capabilities::Connector::new(approver, c.clone());
                if let Some(open) = &self.sign_in {
                    connector = connector.opener(open.clone());
                }
                Arc::new(connector)
            }
            None => approver,
        };
        let mut ix = Interaction {
            info: self.info.clone(),
            ark,
            project,
            session,
            gate: Arc::new(ApprovalGate::new(approver)),
            commands: self.commands.clone(),
            labels: self.labels.clone(),
            status: self.status.clone(),
            on_project: self.on_project.clone(),
            pricing: self.pricing,
            keys: self.keys.clone(),
            history: Vec::new(),
            last_context: None,
            detail: false,
            focus: None,
            names: HashMap::new(),
            present: self.present,
            orchestrator: self.orchestrator.clone(),
            orchs: self.orchs.clone(),
            sign_in: self.sign_in.clone(),
            links: self.links.clone(),
            zone: self.zone.clone(),
            waiting: Vec::new(),
        };
        ix.refresh_names();
        ix.follow_project()?;
        Ok(ix)
    }
}

pub struct Opened {
    pub interaction: Interaction,
    /// A session was continued rather than started.
    pub resumed: bool,
}

/// Parse `<model>`, `<provider>:<model>` or `<provider> <model>`. The provider is a built-in
/// one or a named endpoint (`router:gpt-5`). A bare model keeps the current reasoning provider.
pub fn model_spec(spec: &str, ark: &Ark) -> (String, String) {
    use ark_model::is_provider;
    let spec = spec.trim();
    if let Some((p, m)) = spec.split_once(':') {
        if is_provider(p) {
            return (p.to_string(), m.trim().to_string());
        }
    }
    let parts: Vec<&str> = spec.split_whitespace().collect();
    match parts.as_slice() {
        [p, m, ..] if is_provider(p) => (p.to_string(), m.to_string()),
        _ => {
            let current = ark.router().provider(Role::Reason).describe();
            let provider = current
                .split_whitespace()
                .next()
                .filter(|p| is_provider(p))
                .map(str::to_string)
                .or_else(|| std::env::var("ARK_PROVIDER").ok())
                .unwrap_or_else(|| "ollama".into());
            (provider, spec.to_string())
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppInfo {
    pub name: String,
    pub tagline: Option<String>,
    pub help: Option<String>,
}

/// Handles a front-end holds while a turn runs.
#[derive(Clone)]
pub struct TurnControl {
    /// Trip to stop the turn.
    pub cancel: CancelToken,
    /// Push text here to reach the model at its next step.
    pub steer: Steering,
    /// The turn's events so far, readable while it runs.
    pub recorder: SharedRecorder,
    /// Ask the approval gate before state-changing actions. Off for runs where nobody can
    /// answer: those actions are then refused, and the model is told why.
    pub ask: bool,
}

impl Default for TurnControl {
    fn default() -> Self {
        TurnControl {
            cancel: CancelToken::new(),
            steer: Steering::new(),
            recorder: TurnRecorder::shared(),
            ask: true,
        }
    }
}

impl TurnControl {
    pub fn new() -> Self {
        Self::default()
    }
}

/// How a turn ended.
pub struct TurnEnd {
    pub result: Result<HarnessOutcome, ArkError>,
    pub elapsed_ms: u64,
    /// `3 tools · 25k tokens · 15.0s`, empty when the turn was stopped or failed.
    pub footer: Vec<String>,
    /// Messages sent mid-turn that arrived too late; send them as the next turn.
    pub undelivered: Vec<String>,
    /// The stored turn's index in the session (none when it failed before being recorded).
    pub index: Option<u32>,
}

impl TurnEnd {
    /// The turn stopped because the model's account is out of credit.
    pub fn out_of_credit(&self) -> Option<OutOfCredit> {
        self.result.as_ref().err().and_then(OutOfCredit::of)
    }

    /// Why it failed, as the person reads it ([`crate::labels::person_error`]).
    pub fn error_line(&self) -> Option<String> {
        self.result
            .as_ref()
            .err()
            .map(|e| crate::labels::person_error(&e.to_string()))
    }
}

/// A turn stopped because the model's account has no credit left (the provider answered 402).
/// Every front-end shows `message` (the provider's words) and `hint`; an app that knows the
/// provider can offer its own way to add credit.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutOfCredit {
    /// The provider in the model spec (`openai`, a named endpoint).
    pub provider: String,
    pub message: String,
    /// "Add credit to your OpenRouter account, or pick another model."
    pub hint: String,
}

impl OutOfCredit {
    pub fn of(e: &ArkError) -> Option<OutOfCredit> {
        let (provider, message) = match e {
            ArkError::Runtime(ark_runtime::AgentError::OutOfCredit { provider, message }) => {
                (provider.clone(), message.clone())
            }
            ArkError::Provider(ark_model::ProviderError::OutOfCredit { provider, detail }) => {
                (provider.clone(), detail.clone())
            }
            _ => return None,
        };
        let label = ark_model::endpoints::endpoint(&provider)
            .map(|e| e.label)
            .unwrap_or_else(|| provider.clone());
        Some(OutOfCredit {
            hint: format!("Add credit to your {label} account, or pick another model."),
            provider,
            message,
        })
    }
}

/// What a welcome screen or status line shows.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    pub name: String,
    pub tagline: Option<String>,
    pub model: String,
    pub project: ProjectSummary,
    pub session: SessionSummary,
    pub grants: Vec<String>,
    pub detail: bool,
    pub execution_mode: ark_sdk::prelude::ark_core::ExecutionMode,
    pub execution_mode_revision: u64,
}

/// Turns kept in memory for `/trace`; older ones are read back from the store.
const HISTORY: usize = 16;

pub struct Interaction {
    info: AppInfo,
    ark: Ark,
    project: Project,
    session: Session,
    gate: Arc<ApprovalGate>,
    commands: Vec<Arc<dyn Command>>,
    labels: Labels,
    status: Vec<(String, StatusFn)>,
    on_project: Option<ProjectHook>,
    pricing: Option<Pricing>,
    keys: Vec<(String, String)>,
    /// (stored turn index, record), oldest first.
    history: Vec<(Option<u32>, TurnRecord)>,
    last_context: Option<ContextReport>,
    detail: bool,
    focus: Option<Focus>,
    names: HashMap<String, String>,
    present: bool,
    orchestrator: Option<ark_orchestrate::Orchestrator>,
    /// With [`App::orchestrate`]: every project's orchestrator, shared with sibling conversations.
    orchs: Option<Arc<Orchestrators>>,
    sign_in: Option<crate::capabilities::Opener>,
    /// Other runtimes whose items this one shows too (ADR 0043 step 4), shared with siblings.
    links: Arc<crate::links::Links>,
    /// The person's time zone, when their client said it (shared with siblings).
    zone: Zone,
    /// The items `/waiting` listed last, in its numbers (runtime, id): a number answers what
    /// the person saw.
    pub(crate) waiting: Vec<(Option<String>, String)>,
}

impl Interaction {
    /// The app's orchestrator, when it runs several agents: the current project's when the
    /// app builds one per project.
    pub fn orchestrator(&self) -> Option<&ark_orchestrate::Orchestrator> {
        self.orchestrator.as_ref()
    }

    /// Every orchestrator the app has open, in any project.
    pub fn orchestrators(&self) -> OrchestratorSet {
        OrchestratorSet {
            all: self.orchs.clone(),
            one: self
                .orchs
                .is_none()
                .then(|| self.orchestrator.clone())
                .flatten(),
        }
    }

    /// With [`App::orchestrate`], use the current project's orchestrator (built on first use).
    /// What may run without asking: the session's grants and the open project's lasting ones.
    pub fn grants_view(
        &self,
    ) -> (
        Vec<String>,
        Vec<ark_sdk::prelude::ark_runtime::LastingGrant>,
    ) {
        (
            self.gate.grants(),
            ark_sdk::prelude::ark_runtime::lasting_grants(
                &self.ark.engine().store,
                self.project.id.as_str(),
            ),
        )
    }

    /// Remove a lasting grant of the open project (its class). Returns whether there was one.
    pub fn remove_grant(&self, class: &str) -> bool {
        ark_sdk::prelude::ark_runtime::remove_lasting_grant(
            &self.ark.engine().store,
            self.project.id.as_str(),
            class.trim(),
        )
    }

    fn follow_project(&mut self) -> anyhow::Result<()> {
        // "Always in this project" grants are kept and read for the project now open.
        let store = self.ark.engine().store.clone();
        self.gate.keep_in(store, self.project.id.as_str());
        if let Some(o) = &self.orchs {
            self.orchestrator = Some(o.get(&self.ark, &self.project)?);
        }
        Ok(())
    }

    /// Opens more live conversations of this app (see [`Siblings`]).
    pub fn siblings(&self) -> Siblings {
        Siblings {
            info: self.info.clone(),
            ark: self.ark.clone(),
            commands: self.commands.clone(),
            labels: self.labels.clone(),
            status: self.status.clone(),
            on_project: self.on_project.clone(),
            pricing: self.pricing,
            keys: self.keys.clone(),
            present: self.present,
            orchestrator: self.orchestrator.clone().filter(|_| self.orchs.is_none()),
            orchs: self.orchs.clone(),
            sign_in: self.sign_in.clone(),
            links: self.links.clone(),
            zone: self.zone.clone(),
        }
    }

    /// The other runtimes whose items this one shows too (ADR 0043 step 4).
    pub fn links(&self) -> &Arc<crate::links::Links> {
        &self.links
    }

    /// The person's time zone (IANA, from their client): what they schedule "at 9" runs at
    /// their 9, also when this runtime is on a machine in another zone. Ignored when it is not
    /// a zone this machine knows.
    pub fn set_zone(&self, tz: Option<&str>) {
        let known = tz.filter(|t| {
            ark_orchestrate::When::daily_at("09:00")
                .in_zone(*t)
                .normalized()
                .is_ok()
        });
        *self.zone.lock().unwrap() = known.map(str::to_string);
    }

    pub fn zone(&self) -> Option<String> {
        self.zone.lock().unwrap().clone()
    }

    /// Run on the model chosen last in any of this app's conversations.
    pub fn follow_model(&mut self) {
        self.ark.follow_current_model();
    }
    /// The front-end can show things beside the conversation (see `TurnOptions::present`).
    pub fn set_presentation(&mut self, yes: bool) {
        self.present = yes;
    }

    // ---- accessors ---------------------------------------------------------------------

    pub fn info(&self) -> &AppInfo {
        &self.info
    }
    pub fn app_name(&self) -> &str {
        &self.info.name
    }
    pub fn ark(&self) -> &Ark {
        &self.ark
    }
    pub fn engine(&self) -> &Engine {
        self.ark.engine()
    }
    pub fn project(&self) -> &Project {
        &self.project
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn gate(&self) -> &Arc<ApprovalGate> {
        &self.gate
    }
    pub fn labels(&self) -> &Labels {
        &self.labels
    }
    /// Whether the person asked for the detailed view (`/debug`).
    pub fn detail(&self) -> bool {
        self.detail
    }
    pub fn set_detail(&mut self, on: bool) {
        self.detail = on;
    }
    /// The last request's context breakdown, once a turn has run.
    pub fn last_context(&self) -> Option<&ContextReport> {
        self.last_context.as_ref()
    }
    /// The most recent turn run in this process.
    pub fn last_turn(&self) -> Option<&TurnRecord> {
        self.history.last().map(|(_, r)| r)
    }
    /// Key hints the front-end wants listed in `/help`.
    pub fn set_keys(&mut self, keys: Vec<(String, String)>) {
        self.keys = keys;
    }
    pub fn keys(&self) -> &[(String, String)] {
        &self.keys
    }
    /// What the person is looking at, read into the next turn's context.
    pub fn set_focus(&mut self, focus: Option<Focus>) {
        self.focus = focus.filter(|f| !f.is_empty());
    }
    pub fn status_sections(&self) -> Vec<(String, Vec<Block>)> {
        self.status
            .iter()
            .map(|(t, f)| (t.clone(), f(&self.ark, &self.project)))
            .filter(|(_, b)| !b.is_empty())
            .collect()
    }
    pub fn pricing(&self) -> Option<Pricing> {
        self.pricing
    }

    pub fn summary(&self) -> Summary {
        Summary {
            name: self.info.name.clone(),
            tagline: self.info.tagline.clone(),
            model: self.ark.router().label(),
            project: self.project_summary(&self.project),
            session: SessionSummary::of(self.session.record(), &self.project.name, true),
            grants: self.gate.grants(),
            detail: self.detail,
            execution_mode: self.session.execution_mode(),
            execution_mode_revision: self.session.execution_mode_revision(),
        }
    }

    fn project_summary(&self, p: &Project) -> ProjectSummary {
        ProjectSummary {
            id: p.id.as_str().to_string(),
            name: p.name.clone(),
            target: p.target.clone(),
            current: p.id == self.project.id,
            workspace: p.workspace.clone(),
        }
    }

    // ---- sessions and projects --------------------------------------------------------

    fn reset_turn_state(&mut self) {
        self.gate.revoke_all();
        self.last_context = None;
        self.history.clear();
    }

    /// Start a fresh session in the current project. Session approval grants are cleared;
    /// working state and evidence stay.
    pub fn new_session(&mut self) -> anyhow::Result<()> {
        self.session = self.ark.start_session(&self.project.id)?;
        self.reset_turn_state();
        Ok(())
    }

    /// Continue another session by id or prefix. It restores the session's bounded
    /// carry-over and the project's working state; the transcript is not replayed. Returns
    /// false when none matches.
    pub fn resume(&mut self, id: &str) -> anyhow::Result<bool> {
        let Some(s) = self.ark.open_session(id)? else {
            return Ok(false);
        };
        if s.project() != &self.project.id {
            if let Some(p) = self.engine().store.get_project(s.project())? {
                self.project = p;
                self.focus = None;
                self.follow_project()?;
                self.project_changed();
            }
        }
        self.session = s;
        self.reset_turn_state();
        self.refresh_names();
        crate::attention::seen_chat(self.engine(), self.session.id().as_str());
        Ok(true)
    }

    /// Sessions, most recent first: this project's, or every project's.
    pub fn sessions(&self, all_projects: bool) -> anyhow::Result<Vec<SessionSummary>> {
        let scope = (!all_projects).then_some(&self.project.id);
        list_sessions(self.engine(), scope, self.session.id().as_str())
    }

    /// Rename a session (the current one when `id` is `None`). False when none matches.
    pub fn rename_session(&mut self, id: Option<&str>, title: &str) -> anyhow::Result<bool> {
        let is_current = match id {
            None => true,
            Some(i) => self.session.id().as_str().starts_with(i),
        };
        if is_current {
            self.session.set_title(self.ark.engine(), title)?;
            Ok(true)
        } else {
            Ok(Session::rename(
                self.engine(),
                id.unwrap_or_default(),
                title,
            )?)
        }
    }

    pub fn set_execution_mode(
        &mut self,
        expected_revision: u64,
        mode: ark_sdk::prelude::ark_core::ExecutionMode,
    ) -> anyhow::Result<()> {
        self.session
            .set_execution_mode(self.ark.engine(), expected_revision, mode)?;
        Ok(())
    }

    pub fn projects(&self) -> anyhow::Result<Vec<ProjectSummary>> {
        list_projects(self.engine(), &self.project.id)
    }

    /// The projects a person works in (not orchestrated agents' child projects), newest first.
    fn top_projects(&self) -> anyhow::Result<Vec<Project>> {
        top_projects(self.engine())
    }

    fn top_project(&self, id: &str) -> anyhow::Result<Project> {
        self.top_projects()?
            .into_iter()
            .find(|p| p.id.as_str() == id)
            .ok_or_else(|| anyhow::anyhow!("no project with id `{id}`"))
    }

    /// The sessions of the project with this id, most recent first.
    pub fn project_sessions(&self, id: &str) -> anyhow::Result<Vec<SessionSummary>> {
        project_sessions(self.engine(), id, self.session.id().as_str())
    }

    /// The folder this project's tools (and its agents') work in, and whether it was chosen
    /// (else it is a scratch folder in the store).
    pub fn workspace(&self) -> anyhow::Result<(std::path::PathBuf, bool)> {
        let dir = self.engine().workspace_dir(&self.project.id)?;
        Ok((dir, self.project.workspace.is_some()))
    }

    /// Work in `dir` from now on: this project's tools and its agents' read and write there.
    pub fn set_workspace(&mut self, dir: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
        self.project = self.engine().set_workspace(&self.project.id, dir)?;
        self.project_changed();
        Ok(self.engine().workspace_dir(&self.project.id)?)
    }

    /// Work in another project (created if missing), where its last session left off.
    pub fn switch_project(&mut self, name: &str) -> anyhow::Result<Project> {
        let p = self.ark.find_or_create_project(name.trim(), "")?;
        self.set_project(p.clone())?;
        Ok(p)
    }

    /// Work in the project with this id, where its last session left off.
    pub fn open_project(&mut self, id: &str) -> anyhow::Result<Project> {
        let p = self.top_project(id)?;
        self.set_project(p.clone())?;
        Ok(p)
    }

    /// Add a project and work in it. With `folder`, the project is that folder, named after
    /// it unless `name` is given; a folder that already is a project opens that project.
    /// Without, `name` is opened or created and works in a scratch folder in the store.
    pub fn add_project(
        &mut self,
        name: &str,
        folder: Option<&std::path::Path>,
    ) -> anyhow::Result<Project> {
        let name = name.trim();
        let Some(dir) = folder else {
            anyhow::ensure!(!name.is_empty(), "a project needs a name");
            return self.switch_project(name);
        };
        anyhow::ensure!(dir.is_dir(), "{} is not a folder", dir.display());
        // Without `\\?\` on Windows: the path is shown and compared as text.
        let dir = ark_os::dirs::canonicalize(dir)?;
        let top = self.top_projects()?;
        let path = dir.to_string_lossy();
        let p = match top.iter().find(|p| p.workspace.as_deref() == Some(&*path)) {
            Some(p) => p.clone(),
            None => {
                let base = match name {
                    "" => dir
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "project".into()),
                    n => n.to_string(),
                };
                let p = self.ark.create_project(&unique_name(&base, &top), "")?;
                self.engine().set_workspace(&p.id, &dir)?
            }
        };
        self.set_project(p.clone())?;
        Ok(p)
    }

    /// Rename the project with this id. Names stay unique, since `/project <name>` finds a
    /// project by name.
    pub fn rename_project(&mut self, id: &str, name: &str) -> anyhow::Result<Project> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "a project needs a name");
        let top = self.top_projects()?;
        anyhow::ensure!(
            !top.iter().any(|p| p.name == name && p.id.as_str() != id),
            "a project named {name} already exists"
        );
        let mut p = self.top_project(id)?;
        p.name = name.to_string();
        self.engine()
            .store
            .put_project(&p)
            .map_err(ark_runtime::AgentError::from)?;
        self.project_updated(&p);
        Ok(p)
    }

    /// The folder the project with this id works in, from now on.
    pub fn set_project_folder(
        &mut self,
        id: &str,
        dir: &std::path::Path,
    ) -> anyhow::Result<Project> {
        let p = self.top_project(id)?;
        let p = self.engine().set_workspace(&p.id, dir)?;
        self.project_updated(&p);
        Ok(p)
    }

    fn project_updated(&mut self, p: &Project) {
        if p.id == self.project.id {
            self.project = p.clone();
            self.project_changed();
        }
    }

    /// Work in `project` from now on, in its most recent session (a new one if it has none).
    pub fn set_project(&mut self, project: Project) -> anyhow::Result<()> {
        if project.id == self.project.id {
            self.project = project;
            return Ok(());
        }
        let latest = self.ark.latest_session(&project.id)?;
        self.project = project;
        // What the person looked at belonged to the other project (its Browser page).
        self.focus = None;
        self.follow_project()?;
        match latest {
            Some(s) => {
                self.session = s;
                self.reset_turn_state();
                crate::attention::seen_chat(self.engine(), self.session.id().as_str());
            }
            None => self.new_session()?,
        }
        self.refresh_names();
        self.project_changed();
        Ok(())
    }

    fn project_changed(&self) {
        if let Some(f) = &self.on_project {
            f(&self.project);
        }
    }

    /// Use another model from the next turn on. Returns its description.
    pub fn switch_model(&mut self, spec: &str) -> anyhow::Result<String> {
        let (provider, model) = model_spec(spec, &self.ark);
        let p = ark_model::build(&provider, Some(model))?;
        let d = p.describe();
        self.ark.set_router(ModelRouter::single(p));
        Ok(d)
    }

    /// Run an action directly, recorded as an invocation like any agent call. The person asked
    /// for it (a command, a button), so a state-changing action runs only when `approved`.
    pub async fn run_action(
        &self,
        action: &str,
        input: serde_json::Value,
        approved: bool,
    ) -> anyhow::Result<ark_core::Invocation> {
        Ok(self
            .engine()
            .run_action(&self.project.id, action, input, approved)
            .await?)
    }

    /// Tighten the carried context now.
    pub fn compact(&mut self) -> anyhow::Result<CompactReport> {
        let r = self.session.compact(self.ark.engine())?;
        self.last_context = None;
        Ok(r)
    }

    // ---- commands -----------------------------------------------------------------------

    pub fn find_command(&self, name: &str) -> Option<Arc<dyn Command>> {
        self.commands
            .iter()
            .find(|c| c.name() == name || c.aliases().contains(&name))
            .cloned()
    }

    /// Every command once (a consumer command shadows a built-in of the same name).
    pub fn commands(&self) -> Vec<CommandInfo> {
        let mut seen = std::collections::HashSet::new();
        self.commands
            .iter()
            .filter(|c| seen.insert(c.name().to_string()))
            .map(|c| CommandInfo::of(c.as_ref()))
            .collect()
    }

    /// Run a slash command line. Plain text becomes a `Submit` effect. Errors come back as
    /// an error report, never as a failed call.
    pub async fn execute(&mut self, line: &str) -> Reply {
        let Some((name, args)) = parse(line) else {
            // Nothing typed starts no turn.
            if line.trim().is_empty() {
                return Reply::default();
            }
            return Reply::submit(line.trim());
        };
        let Some(cmd) = self.find_command(name) else {
            return Report::new()
                .warning(format!("Unknown command /{name}. /help lists commands."))
                .into();
        };
        let args = args.to_string();
        match cmd.run(self, &args).await {
            Ok(r) => r,
            Err(e) => Report::new().error(format!("/{name}: {e:#}")).into(),
        }
    }

    // ---- turns --------------------------------------------------------------------------

    fn refresh_names(&mut self) {
        self.names = self
            .engine()
            .store
            .list_artifacts(&self.project.id)
            .map(|a| inspect::artifact_names(&a))
            .unwrap_or_default();
    }

    /// Describe a runtime event in this app's words.
    pub fn describe(&self, ev: &HarnessEvent) -> Option<Activity> {
        activity::describe(&self.labels, &self.names, ev)
    }

    /// Run one turn. `on_event` sees every runtime event, in order, with its description.
    /// The model gets the session's bounded carry-over, never the transcript; the turn and
    /// its trace are stored.
    pub async fn run_turn(
        &mut self,
        input: &str,
        ctl: &TurnControl,
        mut on_event: impl FnMut(&HarnessEvent, Option<&Activity>),
    ) -> TurnEnd {
        self.refresh_names();
        ctl.recorder.lock().unwrap().start(input);
        let opts = TurnOptions {
            cancel: Some(ctl.cancel.clone()),
            approvals: ctl.ask.then(|| self.gate.clone()),
            focus: self.focus.clone(),
            steer: Some(ctl.steer.clone()),
            present: self.present,
            tools: self.turn_tools(),
            execution_mode: self.session.execution_mode(),
        };
        let mut last_context = None;
        let Interaction {
            ark,
            session,
            labels,
            names,
            ..
        } = self;
        let rec = ctl.recorder.clone();
        let turns_before = session.turn_count();
        // The trace is saved as the turn runs, so a turn the app never finished (a crash, the
        // computer stopping) still shows what it did up to there.
        let mut journal = Journal::new(ark.shared_engine(), session.id().as_str(), turns_before);
        let result = ark
            .ask_session(session, input, opts, |ev| {
                rec.lock().unwrap().record(&ev);
                journal.saw(&ev, &rec);
                // The conversation's own requests; the planner's and critic's are separate.
                if let HarnessEvent::Context { report } = &ev {
                    if !matches!(report.role.as_str(), "plan" | "critic") {
                        last_context = Some(report.clone());
                    }
                }
                let a = activity::describe(labels, names, &ev);
                on_event(&ev, a.as_ref());
            })
            .await;
        let (cancelled, error) = match &result {
            Ok(o) => (o.cancelled, None),
            Err(e) => (false, Some(e.to_string())),
        };
        let record = ctl.recorder.lock().unwrap().finish(cancelled, error);
        if last_context.is_some() {
            self.last_context = last_context;
        }
        // A failed turn is stored too (marked incomplete), with its trace.
        let index = (self.session.turn_count() > turns_before)
            .then(|| self.session.turn_count().saturating_sub(1));
        if let Some(i) = index {
            if let Some(stored) =
                crate::trace::for_storage(&record, crate::trace::TraceMode::from_env())
            {
                if let Ok(v) = serde_json::to_value(&stored) {
                    let store = &self.engine().store;
                    let sid = self.session.id().as_str();
                    let _ = store.put_turn_trace(sid, i, &v);
                    // Traces of old turns go; their transcript entries stay.
                    let _ =
                        store.prune_turn_traces(sid, i.saturating_sub(crate::trace::TRACES_KEPT));
                }
            }
        }
        let footer = match &result {
            Ok(o) if !o.cancelled => {
                inspect::footer(&o.metrics, record.elapsed_ms, o.grounded(), o.trivial)
            }
            _ => Vec::new(),
        };
        let elapsed_ms = record.elapsed_ms;
        self.history.push((index, record));
        if self.history.len() > HISTORY {
            self.history.remove(0);
        }
        self.refresh_names();
        TurnEnd {
            result,
            elapsed_ms,
            footer,
            undelivered: ctl.steer.take_undelivered(),
            index,
        }
    }

    // ---- views ----------------------------------------------------------------------------

    /// A turn's record: the last one when `index` is `None`, else the stored turn `index`
    /// of the current session.
    pub fn turn_record(&self, index: Option<u32>) -> Option<TurnRecord> {
        match index {
            None => self.last_turn().cloned(),
            Some(i) => self
                .history
                .iter()
                .find(|(idx, _)| *idx == Some(i))
                .map(|(_, r)| r.clone())
                .or_else(|| load_trace(self.engine(), self.session.id().as_str(), i)),
        }
    }

    pub fn trace(&self, index: Option<u32>) -> Option<Trace> {
        self.turn_record(index).map(|r| trace::interpret(&r))
    }

    /// One page of the stored transcript with each turn's activity, oldest first.
    pub fn transcript(
        &self,
        before: Option<u32>,
        limit: usize,
    ) -> anyhow::Result<Vec<TranscriptTurn>> {
        transcript_page(
            self.engine(),
            &self.labels,
            &self.names,
            &self.session,
            before,
            limit,
        )
    }

    /// Tools the conversation gets beside the app's: with several agents, the orchestrator's
    /// chat tools, so the person asks in words and the chat creates agents and tasks, and the
    /// project's board, so it plans and continues work across chats.
    fn turn_tools(&self) -> Vec<ark_sdk::TurnTools> {
        let chat = self.session.id().as_str();
        self.orchestrator
            .iter()
            .flat_map(|o| {
                [
                    o.chat_tools_in_zone(chat, self.zone().as_deref()),
                    o.board_tools(),
                ]
            })
            .collect()
    }

    /// What the model receives, by source, and what ARK kept out.
    pub async fn context_view(&self) -> anyhow::Result<ContextView> {
        let preview = self
            .ark
            .context_preview_with(&self.session, self.present, &self.turn_tools())
            .await?;
        let (report, source, next) = match &self.last_context {
            Some(r) => (r.clone(), ContextSource::LastRequest, Some(preview.total)),
            None => (preview, ContextSource::Preview, None),
        };
        Ok(ContextView::build(&report, source, next, &self.kept_out()?))
    }

    /// The measurements behind the "kept out of the request" list.
    pub fn kept_out(&self) -> anyhow::Result<KeptOut> {
        // Counted in the database: this runs after every turn, and loading every stored
        // output to measure it made each turn slower as the project grew.
        let rec = self.session.record();
        let engine = self.engine();
        let pid = &self.project.id;
        let stats = engine
            .store
            .project_stats(pid)
            .map_err(ark_runtime::AgentError::from)?;
        let transcript_chars = engine
            .store
            .session_text_chars(self.session.id().as_str(), rec.turn_count)
            .map_err(ark_runtime::AgentError::from)?;
        let state = engine
            .store
            .get_research_state(pid)
            .map_err(ark_runtime::AgentError::from)?
            .unwrap_or_default();
        Ok(KeptOut {
            transcript_turns: rec.turn_count,
            transcript_chars,
            carried_tokens: self
                .session
                .conversation_digest()
                .map(|d| ark_model::estimate_tokens(&d))
                .unwrap_or(0),
            gists: rec.gists.len(),
            dropped_turns: rec.dropped_turns,
            compactions: rec.compactions,
            tool_calls: stats.invocations,
            raw_output_chars: stats.output_chars,
            distilled_chars: stats.card_chars,
            cards: stats.cards,
            artifacts: stats.artifacts,
            state_overflow: StateView::build(state, STATE_DIGEST_ITEMS, &[]).overflow(),
        })
    }

    pub fn usage_view(&self) -> UsageView {
        let u = self.session.usage();
        UsageView {
            model: self.ark.router().describe(),
            turns: u.turns,
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            cached_tokens: u.cached_tokens,
            total_tokens: u.total_tokens(),
            model_calls: u.model_calls,
            tool_calls: u.tool_calls,
            duplicate_tool_calls: u.duplicate_tool_calls,
            peak_prompt_tokens: u.peak_prompt_tokens,
            elapsed_ms: u.elapsed_ms,
            measured: u.measured || u.turns == 0,
            context: self.last_context.as_ref().map(|r| (r.total, r.budget)),
            cost: self
                .pricing
                .map(|p| p.cost(u.prompt_tokens, u.cached_tokens, u.completion_tokens)),
        }
    }

    pub fn state_view(&self) -> anyhow::Result<StateView> {
        let store = &self.engine().store;
        let state = store
            .get_research_state(&self.project.id)
            .map_err(ark_runtime::AgentError::from)?
            .unwrap_or_default();
        let artifacts = store
            .list_artifacts(&self.project.id)
            .map_err(ark_runtime::AgentError::from)?;
        Ok(StateView::build(state, STATE_DIGEST_ITEMS, &artifacts))
    }

    /// Where a resumed session left off: its title and the last exchanges, clipped.
    pub fn recap(&self) -> Report {
        let last = self.transcript(None, 2).unwrap_or_default();
        let session = SessionSummary::of(self.session.record(), &self.project.name, true);
        inspect::recap("Resumed", &session, &last)
    }
}

/// Saves a running turn's trace while it runs (see [`Journal::saw`]). The record is saved
/// `live`; the turn's last save replaces it.
struct Journal {
    engine: Arc<Engine>,
    session: String,
    index: u32,
    mode: crate::trace::TraceMode,
    last: Option<std::time::Instant>,
}

/// How often events that can wait save the trace.
const JOURNAL_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

impl Journal {
    fn new(engine: Arc<Engine>, session: &str, index: u32) -> Journal {
        Journal {
            engine,
            session: session.to_string(),
            index,
            mode: crate::trace::TraceMode::from_env(),
            last: None,
        }
    }

    fn saw(&mut self, ev: &HarnessEvent, rec: &crate::trace::SharedRecorder) {
        // What a person needs to see after a stop is saved at once: results, questions and
        // answers, steering, the end. Anything else (a call starting, streamed text) at most
        // every second; a call under way is also in its own record.
        let due = match ev {
            HarnessEvent::ToolResult { .. }
            | HarnessEvent::Duplicate { .. }
            | HarnessEvent::NeedsApproval { .. }
            | HarnessEvent::ApprovalRequested { .. }
            | HarnessEvent::ApprovalResolved { .. }
            | HarnessEvent::Steered { .. }
            | HarnessEvent::Answer { .. }
            | HarnessEvent::Cancelled
            | HarnessEvent::Interrupted => true,
            HarnessEvent::Context { .. } | HarnessEvent::ModelCall { .. } => false,
            _ => self.last.is_none_or(|t| t.elapsed() >= JOURNAL_EVERY),
        };
        if !due {
            return;
        }
        self.last = Some(std::time::Instant::now());
        let snapshot = rec.lock().unwrap().snapshot();
        if let Some(v) = crate::trace::for_storage(&snapshot, self.mode)
            .and_then(|r| serde_json::to_value(&r).ok())
        {
            if let Err(e) = self
                .engine
                .store
                .put_turn_trace(&self.session, self.index, &v)
            {
                tracing::warn!("a running turn's trace was not saved: {e}");
            }
        }
    }
}

/// A stored turn's record, read straight from the store (no session handle needed).
pub fn load_trace(engine: &Engine, session: &str, index: u32) -> Option<TurnRecord> {
    engine
        .store
        .get_turn_trace(session, index)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// `base`, or `base 2`, `base 3` .. when a project already has that name.
pub(crate) fn unique_name(base: &str, taken: &[Project]) -> String {
    let free = |n: &str| !taken.iter().any(|p| p.name == n);
    if free(base) {
        return base.to_string();
    }
    (2..)
        .map(|i| format!("{base} {i}"))
        .find(|n| free(n))
        .unwrap_or_default()
}
