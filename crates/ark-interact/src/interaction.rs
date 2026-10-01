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
use crate::fmt::{ago, clip, count};
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
        }
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
        let mut commands = self.commands;
        commands.extend(crate::builtins::all());
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
            labels: Labels::new(self.labelers),
            status: self.status,
            on_project: self.on_project,
            pricing: self.pricing.or_else(Pricing::from_env),
            keys: Vec::new(),
            history: Vec::new(),
            last_context: None,
            detail: false,
            focus: None,
            names: HashMap::new(),
        };
        ix.refresh_names();
        Ok(Opened {
            interaction: ix,
            resumed,
        })
    }
}

pub struct Opened {
    pub interaction: Interaction,
    /// A session was continued rather than started.
    pub resumed: bool,
}

/// Parse `<model>`, `<provider>:<model>` or `<provider> <model>`. A bare model keeps the
/// current reasoning provider.
pub fn model_spec(spec: &str, ark: &Ark) -> (String, String) {
    const PROVIDERS: &[&str] = &["ollama", "openai", "anthropic", "mock"];
    let spec = spec.trim();
    if let Some((p, m)) = spec.split_once(':') {
        if PROVIDERS.contains(&p) {
            return (p.to_string(), m.trim().to_string());
        }
    }
    let parts: Vec<&str> = spec.split_whitespace().collect();
    match parts.as_slice() {
        [p, m, ..] if PROVIDERS.contains(p) => (p.to_string(), m.to_string()),
        _ => {
            let current = ark.router().provider(Role::Reason).describe();
            let provider = current
                .split_whitespace()
                .next()
                .filter(|p| PROVIDERS.contains(p))
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

/// What a welcome screen or status line shows.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Summary {
    pub name: String,
    pub tagline: Option<String>,
    pub model: String,
    pub project: ProjectSummary,
    pub session: SessionSummary,
    pub grants: Vec<String>,
    pub detail: bool,
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
}

impl Interaction {
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
    pub fn ark_mut(&mut self) -> &mut Ark {
        &mut self.ark
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
            model: self.ark.router().describe(),
            project: self.project_summary(&self.project),
            session: SessionSummary::of(self.session.record(), &self.project.name, true),
            grants: self.gate.grants(),
            detail: self.detail,
        }
    }

    fn project_summary(&self, p: &Project) -> ProjectSummary {
        ProjectSummary {
            id: p.id.as_str().to_string(),
            name: p.name.clone(),
            target: p.target.clone(),
            current: p.id == self.project.id,
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
                self.project_changed();
            }
        }
        self.session = s;
        self.reset_turn_state();
        self.refresh_names();
        Ok(true)
    }

    /// Sessions, most recent first: this project's, or every project's.
    pub fn sessions(&self, all_projects: bool) -> anyhow::Result<Vec<SessionSummary>> {
        let names: HashMap<String, String> = self
            .engine()
            .store
            .list_projects()
            .map_err(ark_runtime::AgentError::from)?
            .into_iter()
            .map(|p| (p.id.as_str().to_string(), p.name))
            .collect();
        let scope = (!all_projects).then_some(&self.project.id);
        let current = self.session.id().clone();
        Ok(Session::list(self.engine(), scope)?
            .iter()
            .map(|r| {
                let pname = names.get(r.project.as_str()).cloned().unwrap_or_default();
                SessionSummary::of(r, &pname, r.id == current)
            })
            .collect())
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

    pub fn projects(&self) -> anyhow::Result<Vec<ProjectSummary>> {
        Ok(self
            .engine()
            .store
            .list_projects()
            .map_err(ark_runtime::AgentError::from)?
            .iter()
            .map(|p| self.project_summary(p))
            .collect())
    }

    /// Work in another project (created if missing), in a fresh session there.
    pub fn switch_project(&mut self, name: &str) -> anyhow::Result<Project> {
        let p = self.ark.find_or_create_project(name.trim(), "")?;
        self.set_project(p.clone())?;
        Ok(p)
    }

    /// Work in `project` from now on, in a fresh session there.
    pub fn set_project(&mut self, project: Project) -> anyhow::Result<()> {
        self.project = project;
        self.new_session()?;
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
        let result = ark
            .ask_session(session, input, opts, |ev| {
                rec.lock().unwrap().record(&ev);
                if let HarnessEvent::Context { report } = &ev {
                    last_context = Some(report.clone());
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
            if let Ok(v) = serde_json::to_value(&record) {
                let _ = self
                    .engine()
                    .store
                    .put_turn_trace(self.session.id().as_str(), i, &v);
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
        let sid = self.session.id().as_str().to_string();
        Ok(self
            .session
            .transcript_page(self.engine(), before, limit)?
            .iter()
            .map(|t| {
                let rec = load_trace(self.engine(), &sid, t.index);
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
                                ev => self.describe(ev),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                TranscriptTurn::of(t, activity, rec.is_some())
            })
            .collect())
    }

    /// What the model receives, by source, and what ARK kept out.
    pub async fn context_view(&self) -> anyhow::Result<ContextView> {
        let preview = self.ark.context_preview(&self.session).await?;
        let (report, source, next) = match &self.last_context {
            Some(r) => (r.clone(), ContextSource::LastRequest, Some(preview.total)),
            None => (preview, ContextSource::Preview, None),
        };
        Ok(ContextView::build(&report, source, next, &self.kept_out()?))
    }

    /// The measurements behind the "kept out of the request" list.
    pub fn kept_out(&self) -> anyhow::Result<KeptOut> {
        let rec = self.session.record();
        let engine = self.engine();
        let pid = &self.project.id;
        let transcript = self.session.transcript(engine, None)?;
        let invs = engine
            .store
            .list_invocations(pid)
            .map_err(ark_runtime::AgentError::from)?;
        let cards = engine
            .store
            .list_evidence_cards(pid)
            .map_err(ark_runtime::AgentError::from)?;
        let artifacts = engine
            .store
            .list_artifacts(pid)
            .map_err(ark_runtime::AgentError::from)?;
        let state = engine
            .store
            .get_research_state(pid)
            .map_err(ark_runtime::AgentError::from)?
            .unwrap_or_default();
        let k = KeptOut {
            transcript_turns: rec.turn_count,
            transcript_chars: transcript
                .iter()
                .map(|t| t.input.len() + t.answer.len())
                .sum(),
            carried_tokens: self
                .session
                .conversation_digest()
                .map(|d| ark_model::estimate_tokens(&d))
                .unwrap_or(0),
            gists: rec.gists.len(),
            dropped_turns: rec.dropped_turns,
            compactions: rec.compactions,
            tool_calls: invs.len(),
            raw_output_chars: invs.iter().map(|i| i.output.to_string().len()).sum(),
            distilled_chars: cards
                .iter()
                .map(|c| c.summary.len() + c.identifiers.iter().map(|x| x.len()).sum::<usize>())
                .sum(),
            cards: cards.len(),
            artifacts: artifacts.len(),
            state_overflow: StateView::build(state, STATE_DIGEST_ITEMS, &[]).overflow(),
        };
        Ok(k)
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
        let rec = self.session.record();
        let title = if rec.title.is_empty() {
            rec.id.short().to_string()
        } else {
            clip(&rec.title, 60)
        };
        let mut r = Report::new().success(format!(
            "Resumed {title} · {} · {}",
            count(rec.turn_count as u64, "turn"),
            ago(rec.updated_at)
        ));
        for t in self
            .session
            .transcript(self.engine(), Some(2))
            .unwrap_or_default()
        {
            r = r.text(format!("› {}", clip(&t.input, 100)));
            r = r.note(if t.cancelled {
                "  (stopped)".to_string()
            } else if t.incomplete {
                format!(
                    "  (did not finish: {})",
                    t.error.as_deref().unwrap_or("unknown reason")
                )
            } else {
                format!("  {}", clip(&t.answer, 160))
            });
        }
        r.note("The agent continues from its working state; the transcript is not replayed.")
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
