//! The orchestrator: agents, tasks and the scheduler that runs them.
//!
//! `run(agent, task)` is the whole contract. The orchestrator resolves the agent's models
//! (see [`crate::models`]), waits for what the task depends on, for the agent to be free
//! and for capacity on the agent's account, then runs one harness turn in the agent's own
//! project with a brief of the task. Everything the agent does is recorded on the task and
//! in the event stream.
//!
//! Scheduling rules:
//!
//! | Rule | Why |
//! |---|---|
//! | One task at a time per agent | an agent has one context and one vendor session |
//! | At most `max_concurrent` running tasks per account | a subscription allows only so many requests at once |
//! | At most `max_parallel` running tasks overall | a bound on local work |
//! | A task waiting on tasks it delegated gives back its capacity | a parent never blocks its own children |
//! | Delegating up the chain that is waiting is refused | it would wait on itself |

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use ark_core::{Project, ProjectId, RuleBinding, RuleScope};
use ark_model::estimate_tokens;
use ark_runtime::rules::{select, RULES_BUDGET};
use ark_runtime::{
    AgentProfile, ApprovalDecision, ApprovalGate, ApprovalRequest, Approver, CancelToken, Engine,
    Harness, HarnessEvent, RuleBook, RulesTool, Session, Steering,
};
use ark_sdk::Ark;
use ark_tools::{action_matches, Registry};
use tokio::sync::{broadcast, watch, OwnedSemaphorePermit, Semaphore};

use crate::brief::{clip, Brief};
use crate::event::{EventKind, OrchEvent};
use crate::models::{AgentModels, ModelResolver, Models, ModelsBuilder};
use crate::record::{
    now_ms, AgentRecord, AgentState, ArtifactRef, Mail, Origin, TaskRecord, TaskState,
    ToolCallRecord, Usage, TOOL_CALLS_KEPT,
};
use crate::spec::{AgentSpec, TaskSpec};
use crate::tools::{self, AgentTools};
use crate::OrchError;

const AGENT: &str = "orch_agent";
const TASK: &str = "orch_task";
const MESSAGE: &str = "orch_message";

/// Runs agents and tasks in one project. Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct Orchestrator {
    pub(crate) inner: Arc<Inner>,
}

/// Builder for [`Orchestrator`].
pub struct OrchestratorBuilder<'a> {
    ark: &'a Ark,
    project: Project,
    models: Option<Arc<dyn ModelResolver>>,
    configure: Option<Box<dyn FnOnce(ModelsBuilder) -> ModelsBuilder + 'a>>,
    approvals: Option<Arc<ApprovalGate>>,
    max_parallel: usize,
    profiles: Vec<NamedProfile>,
}

/// A profile an application offers by name: agents choose it with
/// [`AgentSpec::profile_name`], from code or from a front-end.
#[derive(Clone)]
pub struct NamedProfile {
    pub name: String,
    /// One line on what an agent on it is good for, shown where a profile is chosen.
    pub summary: String,
    pub profile: AgentProfile,
}

impl<'a> OrchestratorBuilder<'a> {
    /// Resolve agents' models with this instead of [`Models`].
    pub fn models(mut self, m: Arc<dyn ModelResolver>) -> Self {
        self.models = Some(m);
        self
    }
    /// Adjust the default [`Models`] (capacities, session policy, named providers). The
    /// app's router and a store ledger are already set.
    pub fn configure_models(mut self, f: impl FnOnce(ModelsBuilder) -> ModelsBuilder + 'a) -> Self {
        self.configure = Some(Box::new(f));
        self
    }
    /// Where agents' state-changing actions go for approval (an agent with `allow_mutate`
    /// skips it; without a gate they are refused).
    pub fn approvals(mut self, gate: Arc<ApprovalGate>) -> Self {
        self.approvals = Some(gate);
        self
    }
    /// Offer `profile` under `name`. Agents start from `ark`'s profile unless their spec
    /// names one of these (or carries a profile of its own).
    pub fn profile(
        mut self,
        name: impl Into<String>,
        summary: impl Into<String>,
        profile: AgentProfile,
    ) -> Self {
        let name = name.into();
        self.profiles.retain(|p| p.name != name);
        self.profiles.push(NamedProfile {
            name,
            summary: summary.into(),
            profile,
        });
        self
    }
    /// Tasks running at once across all accounts (default 8).
    pub fn max_parallel(mut self, n: usize) -> Self {
        self.max_parallel = n.max(1);
        self
    }

    /// Open the orchestrator: agents and tasks stored for the project are loaded, and tasks a
    /// stopped process left running are marked failed (see [`Orchestrator::retry`]).
    pub fn build(self) -> Result<Orchestrator, OrchError> {
        let engine = self.ark.shared_engine();
        let models: Arc<dyn ModelResolver> = match self.models {
            Some(m) => m,
            None => {
                let mut b = Models::builder()
                    .default_router(self.ark.router().clone_view())
                    .subscriptions(self.ark.subscriptions().clone());
                if let Some(f) = self.configure {
                    b = f(b);
                }
                Arc::new(b.build())
            }
        };
        let settings = self.ark.settings();
        let (events, _) = broadcast::channel(1024);
        let project = self.project;
        let inner = Arc::new_cyclic(|me| Inner {
            me: me.clone(),
            stream: format!("orch:{}", project.id),
            engine,
            base: self.ark.profile().clone(),
            profiles: self.profiles,
            context_budget: settings.context_budget,
            max_steps: settings.max_steps,
            planner: settings.planner,
            critic: settings.critic,
            project,
            models,
            approvals: self.approvals,
            max_parallel: self.max_parallel,
            global: Arc::new(Semaphore::new(self.max_parallel)),
            accounts: Mutex::new(HashMap::new()),
            state: Mutex::new(State::default()),
            events,
        });
        inner.load()?;
        Ok(Orchestrator { inner })
    }
}

impl Orchestrator {
    /// An orchestrator over `ark`'s store, tools and settings, working in `project`. Agents
    /// without a model use `ark`'s router; agents without their own profile start from
    /// `ark`'s profile.
    pub fn builder<'a>(ark: &'a Ark, project: &Project) -> OrchestratorBuilder<'a> {
        OrchestratorBuilder {
            ark,
            project: project.clone(),
            models: None,
            configure: None,
            approvals: None,
            max_parallel: 8,
            profiles: Vec::new(),
        }
    }

    /// The profiles agents may name (see [`OrchestratorBuilder::profile`]).
    pub fn profiles(&self) -> &[NamedProfile] {
        &self.inner.profiles
    }

    pub fn project(&self) -> &Project {
        &self.inner.project
    }

    pub fn models(&self) -> &Arc<dyn ModelResolver> {
        &self.inner.models
    }

    /// Add an agent, or update the spec of an existing one (a terminated agent comes back).
    pub async fn spawn(&self, spec: AgentSpec) -> Result<AgentRecord, OrchError> {
        self.inner.spawn(spec).await
    }

    pub fn agent(&self, name: &str) -> Option<AgentRecord> {
        let st = self.inner.state.lock().unwrap();
        st.agents.get(name).map(|a| a.record.clone())
    }

    /// Every agent, in the order they were spawned.
    pub fn agents(&self) -> Vec<AgentRecord> {
        let st = self.inner.state.lock().unwrap();
        let mut v: Vec<AgentRecord> = st.agents.values().map(|a| a.record.clone()).collect();
        v.sort_by_key(|a| (a.created_ms, a.name.clone()));
        v
    }

    /// Create a task for `agent` and schedule it. Returns its id at once.
    pub fn submit(&self, agent: &str, spec: TaskSpec) -> Result<String, OrchError> {
        let id = self
            .inner
            .create_task(agent, spec, Origin::External, None)?;
        self.inner.start(&id);
        Ok(id)
    }

    /// Run a task on `agent` and wait for it to end (done, failed or cancelled).
    pub async fn run(&self, agent: &str, spec: TaskSpec) -> Result<TaskRecord, OrchError> {
        let id = self.submit(agent, spec)?;
        self.wait(&id).await
    }

    /// Wait for a task to end.
    pub async fn wait(&self, task: &str) -> Result<TaskRecord, OrchError> {
        if !self.inner.state.lock().unwrap().tasks.contains_key(task) {
            return Err(OrchError::UnknownTask(task.into()));
        }
        Ok(self.inner.wait_final(task).await)
    }

    pub fn task(&self, id: &str) -> Option<TaskRecord> {
        let st = self.inner.state.lock().unwrap();
        st.tasks.get(id).map(|t| t.record.clone())
    }

    /// Every task, oldest first.
    pub fn tasks(&self) -> Vec<TaskRecord> {
        let st = self.inner.state.lock().unwrap();
        let mut v: Vec<TaskRecord> = st.tasks.values().map(|t| t.record.clone()).collect();
        v.sort_by_key(|t| (t.created_ms, t.id.clone()));
        v
    }

    /// Stop a task and every task it delegated that has not ended.
    pub fn cancel(&self, task: &str) -> Result<(), OrchError> {
        if !self.inner.state.lock().unwrap().tasks.contains_key(task) {
            return Err(OrchError::UnknownTask(task.into()));
        }
        self.inner.cancel_tree(task, Stop::Cancel);
        Ok(())
    }

    /// Stop scheduling work for `agent`. A task it is running stops at the next step and is
    /// suspended; it runs again, from the agent's working state, on [`resume`](Self::resume).
    pub fn suspend(&self, agent: &str) -> Result<(), OrchError> {
        self.inner.suspend(agent)
    }

    pub fn resume(&self, agent: &str) -> Result<(), OrchError> {
        self.inner.resume(agent)
    }

    /// Cancel everything `agent` has open, close its model session, and take no more work
    /// for it. Its records and store stay.
    pub async fn terminate(&self, agent: &str) -> Result<(), OrchError> {
        self.inner.terminate(agent).await
    }

    /// Run a failed or cancelled task again (one more attempt).
    pub fn retry(&self, task: &str) -> Result<(), OrchError> {
        self.inner.retry(task)
    }

    /// Send `agent` a message from the application (`from` names the sender, for example
    /// "user").
    pub fn message(&self, from: &str, agent: &str, text: &str) -> Result<bool, OrchError> {
        self.inner.send(from, agent, text, None)
    }

    /// Live events, including streamed text (which is not stored).
    pub fn subscribe(&self) -> broadcast::Receiver<OrchEvent> {
        self.inner.events.subscribe()
    }

    /// Stored events after `seq`, oldest first.
    pub fn events(&self, after: u64, limit: usize) -> Result<Vec<OrchEvent>, OrchError> {
        self.inner.events_after(after, limit)
    }

    /// Every stored event of one task: its lifecycle, its agent's runtime events, the
    /// delegations it made.
    pub fn task_trace(&self, task: &str) -> Result<Vec<OrchEvent>, OrchError> {
        let mut out = Vec::new();
        let mut after = 0;
        loop {
            let batch = self.inner.events_after(after, 1000)?;
            let Some(last) = batch.last() else { break };
            after = last.seq;
            out.extend(batch.into_iter().filter(|e| e.kind.concerns_task(task)));
        }
        Ok(out)
    }

    /// The rule sets in the store; bind them to the project, an agent or a task.
    pub fn rule_book(&self) -> RuleBook<'_> {
        RuleBook::new(&self.inner.engine.store)
    }

    /// The live graph: agents, tasks, edges, sessions, capacity.
    pub fn graph(&self) -> crate::Graph {
        crate::graph::build(&self.inner)
    }

    /// Release vendor sessions idle longer than the session policy allows.
    pub async fn sweep_idle(&self) -> usize {
        self.inner.models.sweep_idle().await
    }

    /// Run [`sweep_idle`](Self::sweep_idle) every `every` while the orchestrator lives.
    pub fn spawn_sweeper(&self, every: Duration) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(inner) = weak.upgrade() else { break };
                inner.models.sweep_idle().await;
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stop {
    Cancel,
    Suspend,
    Limit(String),
}

#[derive(Default)]
struct Slots {
    account: Option<OwnedSemaphorePermit>,
    global: Option<OwnedSemaphorePermit>,
    account_name: String,
    /// Delegation waits in progress; capacity is given back while any runs.
    waiters: usize,
}

pub(crate) struct AgentRt {
    pub record: AgentRecord,
    lock: Arc<tokio::sync::Mutex<()>>,
    status: watch::Sender<AgentState>,
    steering: Option<Steering>,
    pub models: Option<Arc<AgentModels>>,
}

pub(crate) struct TaskRt {
    pub record: TaskRecord,
    cancel: CancelToken,
    stop: Option<Stop>,
    done: watch::Sender<bool>,
    slots: Arc<tokio::sync::Mutex<Slots>>,
    pub waiting_for: Vec<String>,
    delegations: u32,
}

#[derive(Default)]
pub(crate) struct State {
    pub agents: HashMap<String, AgentRt>,
    pub tasks: HashMap<String, TaskRt>,
    pub messages: Vec<crate::graph::MessageEdge>,
}

pub(crate) struct Inner {
    me: Weak<Inner>,
    pub engine: Arc<Engine>,
    base: AgentProfile,
    profiles: Vec<NamedProfile>,
    context_budget: u32,
    max_steps: usize,
    planner: bool,
    critic: bool,
    pub project: Project,
    pub models: Arc<dyn ModelResolver>,
    approvals: Option<Arc<ApprovalGate>>,
    pub max_parallel: usize,
    global: Arc<Semaphore>,
    accounts: Mutex<HashMap<String, Arc<Semaphore>>>,
    pub state: Mutex<State>,
    events: broadcast::Sender<OrchEvent>,
    stream: String,
}

enum Attempt {
    Done(String),
    Stopped(Stop),
    Failed(String),
}

struct Final {
    state: TaskState,
    result: Option<String>,
    error: Option<String>,
}

impl Final {
    fn done(r: String) -> Self {
        Final {
            state: TaskState::Done,
            result: Some(r),
            error: None,
        }
    }
    fn failed(e: impl Into<String>) -> Self {
        Final {
            state: TaskState::Failed,
            result: None,
            error: Some(e.into()),
        }
    }
    fn cancelled(why: impl Into<String>) -> Self {
        Final {
            state: TaskState::Cancelled,
            result: None,
            error: Some(why.into()),
        }
    }
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl Inner {
    fn arc(&self) -> Arc<Inner> {
        self.me
            .upgrade()
            .expect("the orchestrator is alive while it runs")
    }

    fn lane(&self, agent: &str) -> String {
        format!("{}/{agent}", self.project.id)
    }

    // ---- persistence and events --------------------------------------------------

    pub(crate) fn emit(&self, kind: EventKind) {
        let mut ev = OrchEvent {
            seq: 0,
            at_ms: now_ms(),
            project: self.project.id.to_string(),
            kind,
        };
        if ev.kind.stored() {
            match serde_json::to_value(&ev) {
                Ok(v) => match self.engine.store.append_event(&self.stream, &v) {
                    Ok(seq) => ev.seq = seq,
                    Err(e) => tracing::warn!("could not store an orchestration event: {e}"),
                },
                Err(e) => tracing::warn!("could not encode an orchestration event: {e}"),
            }
        }
        let _ = self.events.send(ev);
    }

    fn events_after(&self, after: u64, limit: usize) -> Result<Vec<OrchEvent>, OrchError> {
        let mut out = Vec::new();
        for (seq, v) in self.engine.store.list_events(&self.stream, after, limit)? {
            let mut e: OrchEvent = serde_json::from_value(v)?;
            e.seq = seq;
            out.push(e);
        }
        Ok(out)
    }

    fn save_agent(&self, r: &AgentRecord) {
        let id = format!("{}/{}", self.project.id, r.name);
        match serde_json::to_value(r) {
            Ok(v) => {
                if let Err(e) =
                    self.engine
                        .store
                        .put_record(AGENT, &id, self.project.id.as_str(), &v)
                {
                    tracing::warn!(agent = %r.name, "could not save the agent: {e}");
                }
            }
            Err(e) => tracing::warn!("could not encode an agent: {e}"),
        }
    }

    fn save_task(&self, r: &TaskRecord) {
        match serde_json::to_value(r) {
            Ok(v) => {
                if let Err(e) =
                    self.engine
                        .store
                        .put_record(TASK, &r.id, self.project.id.as_str(), &v)
                {
                    tracing::warn!(task = %r.id, "could not save the task: {e}");
                }
            }
            Err(e) => tracing::warn!("could not encode a task: {e}"),
        }
    }

    /// Change a task under the lock, then store it.
    fn update_task(&self, id: &str, f: impl FnOnce(&mut TaskRecord)) -> Option<TaskRecord> {
        let rec = {
            let mut st = self.state.lock().unwrap();
            let t = st.tasks.get_mut(id)?;
            f(&mut t.record);
            t.record.clone()
        };
        self.save_task(&rec);
        Some(rec)
    }

    fn update_agent(&self, name: &str, f: impl FnOnce(&mut AgentRecord)) -> Option<AgentRecord> {
        let rec = {
            let mut st = self.state.lock().unwrap();
            let a = st.agents.get_mut(name)?;
            f(&mut a.record);
            a.record.updated_ms = now_ms();
            a.record.clone()
        };
        self.save_agent(&rec);
        Some(rec)
    }

    fn set_agent_state(&self, name: &str, state: AgentState) {
        let changed = {
            let mut st = self.state.lock().unwrap();
            match st.agents.get_mut(name) {
                Some(a) if a.record.state != state => {
                    a.record.state = state;
                    a.record.updated_ms = now_ms();
                    a.status.send_replace(state);
                    Some(a.record.clone())
                }
                _ => None,
            }
        };
        if let Some(r) = changed {
            self.save_agent(&r);
            self.emit(EventKind::AgentState {
                agent: name.to_string(),
                state,
            });
        }
    }

    /// Load the project's agents and tasks. Tasks left open by a process that is gone are
    /// marked failed.
    fn load(&self) -> Result<(), OrchError> {
        let store = &self.engine.store;
        let pid = std::process::id();
        let mut interrupted = Vec::new();
        {
            let mut st = self.state.lock().unwrap();
            for v in store.list_records(AGENT, Some(self.project.id.as_str()))? {
                let Ok(mut r) = serde_json::from_value::<AgentRecord>(v) else {
                    continue;
                };
                if matches!(r.state, AgentState::Running | AgentState::Waiting) {
                    r.state = AgentState::Idle;
                }
                r.current_task = None;
                let (status, _) = watch::channel(r.state);
                st.agents.insert(
                    r.name.clone(),
                    AgentRt {
                        record: r,
                        lock: Arc::new(tokio::sync::Mutex::new(())),
                        status,
                        steering: None,
                        models: None,
                    },
                );
            }
            for v in store.list_records(TASK, Some(self.project.id.as_str()))? {
                let Ok(mut r) = serde_json::from_value::<TaskRecord>(v) else {
                    continue;
                };
                if !r.state.is_final() {
                    let alive = r.pid.is_some_and(|p| p != pid && process_alive(p));
                    if !alive {
                        r.state = TaskState::Failed;
                        r.error = Some(INTERRUPTED.into());
                        r.waiting_on = None;
                        r.finished_ms = Some(now_ms());
                        r.pid = None;
                        interrupted.push(r.clone());
                    }
                }
                let (done, _) = watch::channel(r.state.is_final());
                st.tasks.insert(
                    r.id.clone(),
                    TaskRt {
                        record: r,
                        cancel: CancelToken::new(),
                        stop: None,
                        done,
                        slots: Default::default(),
                        waiting_for: Vec::new(),
                        delegations: 0,
                    },
                );
            }
            for v in store.list_records(MESSAGE, Some(self.project.id.as_str()))? {
                if let Ok(m) = serde_json::from_value(v) {
                    st.messages.push(m);
                }
            }
        }
        for r in interrupted {
            self.save_task(&r);
            self.emit(EventKind::TaskFinished {
                task: r.id.clone(),
                state: r.state,
                result: None,
                error: r.error.clone(),
                usage: r.usage,
                run_ms: r.run_ms,
            });
        }
        Ok(())
    }

    // ---- agents ------------------------------------------------------------------

    async fn spawn(&self, spec: AgentSpec) -> Result<AgentRecord, OrchError> {
        if !valid_name(&spec.name) {
            return Err(OrchError::NotAllowed(format!(
                "`{}` is not a valid agent name (letters, digits, - _ .)",
                spec.name
            )));
        }
        if let Some(name) = &spec.profile_name {
            if !self.profiles.iter().any(|p| &p.name == name) {
                let offered: Vec<&str> = self.profiles.iter().map(|p| p.name.as_str()).collect();
                return Err(OrchError::NotAllowed(format!(
                    "no profile `{name}` (offered: {})",
                    if offered.is_empty() {
                        "none".to_string()
                    } else {
                        offered.join(", ")
                    }
                )));
            }
        }
        let models = Arc::new(
            self.models
                .resolve(&self.lane(&spec.name), spec.model.as_deref())
                .await?,
        );
        if let Some(s) = &models.session {
            let weak = self.me.clone();
            let agent = spec.name.clone();
            s.set_listener(Arc::new(move |e| {
                if let Some(inner) = weak.upgrade() {
                    inner.emit(EventKind::Session {
                        agent: agent.clone(),
                        event: e.clone(),
                    });
                }
            }));
        }
        let existing = self
            .state
            .lock()
            .unwrap()
            .agents
            .get(&spec.name)
            .map(|a| a.record.clone());
        let record = match existing {
            Some(mut r) => {
                r.spec = spec.clone();
                r.model = models.model.clone();
                r.account = models.account.clone();
                if r.state == AgentState::Terminated {
                    r.state = AgentState::Idle;
                }
                r.updated_ms = now_ms();
                r
            }
            None => {
                // No folder of its own: it works in the project's (see `workspace_dir`).
                let scope =
                    Project::new(&spec.name, &spec.role).with_parent(self.project.id.clone());
                self.engine.store.put_project(&scope)?;
                AgentRecord {
                    name: spec.name.clone(),
                    project: self.project.id.to_string(),
                    scope: scope.id.to_string(),
                    spec: spec.clone(),
                    state: AgentState::Idle,
                    current_task: None,
                    session: None,
                    model: models.model.clone(),
                    account: models.account.clone(),
                    usage: Usage::default(),
                    tasks_done: 0,
                    tasks_failed: 0,
                    last_context: None,
                    inbox: Vec::new(),
                    created_ms: now_ms(),
                    updated_ms: now_ms(),
                }
            }
        };
        {
            let mut st = self.state.lock().unwrap();
            match st.agents.get_mut(&spec.name) {
                Some(a) => {
                    a.record = record.clone();
                    a.models = Some(models.clone());
                    a.status.send_replace(record.state);
                }
                None => {
                    let (status, _) = watch::channel(record.state);
                    st.agents.insert(
                        spec.name.clone(),
                        AgentRt {
                            record: record.clone(),
                            lock: Arc::new(tokio::sync::Mutex::new(())),
                            status,
                            steering: None,
                            models: Some(models.clone()),
                        },
                    );
                }
            }
        }
        self.save_agent(&record);
        self.emit(EventKind::AgentSpawned {
            agent: spec.name.clone(),
            parent: spec.parent.clone(),
            model: models.model.clone(),
        });
        Ok(record)
    }

    async fn models_for(&self, agent: &str) -> Result<Arc<AgentModels>, OrchError> {
        let (have, spec) = {
            let st = self.state.lock().unwrap();
            let a = st
                .agents
                .get(agent)
                .ok_or_else(|| OrchError::UnknownAgent(agent.into()))?;
            (a.models.clone(), a.record.spec.clone())
        };
        if let Some(m) = have {
            return Ok(m);
        }
        // Loaded from the store: resolve, and wire the session events, as spawn does.
        self.spawn(spec).await?;
        let st = self.state.lock().unwrap();
        st.agents
            .get(agent)
            .and_then(|a| a.models.clone())
            .ok_or_else(|| OrchError::UnknownAgent(agent.into()))
    }

    fn suspend(&self, agent: &str) -> Result<(), OrchError> {
        let current = {
            let st = self.state.lock().unwrap();
            let a = st
                .agents
                .get(agent)
                .ok_or_else(|| OrchError::UnknownAgent(agent.into()))?;
            if a.record.state == AgentState::Terminated {
                return Err(OrchError::Terminated(agent.into()));
            }
            a.record.current_task.clone()
        };
        self.set_agent_state(agent, AgentState::Suspended);
        if let Some(t) = current {
            self.stop(&t, Stop::Suspend);
        }
        Ok(())
    }

    fn resume(&self, agent: &str) -> Result<(), OrchError> {
        let state = {
            let st = self.state.lock().unwrap();
            st.agents
                .get(agent)
                .ok_or_else(|| OrchError::UnknownAgent(agent.into()))?
                .record
                .state
        };
        if state == AgentState::Suspended {
            self.set_agent_state(agent, AgentState::Idle);
        }
        Ok(())
    }

    async fn terminate(&self, agent: &str) -> Result<(), OrchError> {
        let open: Vec<String> = {
            let st = self.state.lock().unwrap();
            if !st.agents.contains_key(agent) {
                return Err(OrchError::UnknownAgent(agent.into()));
            }
            st.tasks
                .values()
                .filter(|t| t.record.owner == agent && !t.record.state.is_final())
                .map(|t| t.record.id.clone())
                .collect()
        };
        self.set_agent_state(agent, AgentState::Terminated);
        for t in open {
            self.cancel_tree(&t, Stop::Cancel);
        }
        self.models.close(&self.lane(agent)).await;
        Ok(())
    }

    // ---- tasks -------------------------------------------------------------------

    fn scope_of(&self, agent: &str) -> Option<ProjectId> {
        let st = self.state.lock().unwrap();
        st.agents
            .get(agent)
            .map(|a| ProjectId(a.record.scope.clone()))
    }

    /// Give `to` the artifact rows (the bytes are shared) of `ids`, resolved in `from`.
    fn share_artifacts(
        &self,
        from: &ProjectId,
        to: &ProjectId,
        ids: &[String],
    ) -> Result<Vec<ArtifactRef>, OrchError> {
        let mut out = Vec::new();
        for id in ids {
            let a = self
                .engine
                .store
                .resolve_artifact(from, id)?
                .ok_or_else(|| OrchError::NotAllowed(format!("no artifact `{id}` to pass on")))?;
            self.engine.store.put_artifact(to, &a)?;
            out.push(ArtifactRef {
                id: a.id.to_string(),
                name: a.name.clone(),
                kind: a.kind.as_tag(),
            });
        }
        Ok(out)
    }

    pub(crate) fn create_task(
        &self,
        owner: &str,
        spec: TaskSpec,
        origin: Origin,
        parent: Option<&str>,
    ) -> Result<String, OrchError> {
        let (agent_budget, state) = {
            let st = self.state.lock().unwrap();
            let a = st
                .agents
                .get(owner)
                .ok_or_else(|| OrchError::UnknownAgent(owner.into()))?;
            for d in &spec.after {
                if !st.tasks.contains_key(d) {
                    return Err(OrchError::UnknownTask(d.clone()));
                }
            }
            (a.record.spec.budget.clone(), a.record.state)
        };
        if state == AgentState::Terminated {
            return Err(OrchError::Terminated(owner.into()));
        }
        if spec.objective.trim().is_empty() {
            return Err(OrchError::NotAllowed("a task needs an objective".into()));
        }
        let id = format!("t-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let source = match &origin {
            Origin::Agent { agent, .. } => self.scope_of(agent).unwrap_or(self.project.id.clone()),
            Origin::External => self.project.id.clone(),
        };
        let dest = self
            .scope_of(owner)
            .ok_or_else(|| OrchError::UnknownAgent(owner.into()))?;
        let inputs = self.share_artifacts(&source, &dest, &spec.artifacts)?;
        let book = RuleBook::new(&self.engine.store);
        for b in &spec.rules {
            book.bind(&RuleBinding {
                ruleset: b.ruleset.clone(),
                version: b.version,
                scope: RuleScope::Task(id.clone()),
            })?;
        }
        let record = TaskRecord {
            id: id.clone(),
            project: self.project.id.to_string(),
            owner: owner.to_string(),
            objective: spec.objective.clone(),
            context: spec.context.clone(),
            origin: origin.clone(),
            parent: parent.map(str::to_string),
            children: Vec::new(),
            after: spec.after.clone(),
            inputs,
            state: if spec.after.is_empty() {
                TaskState::Queued
            } else {
                TaskState::Pending
            },
            waiting_on: None,
            attempts: 0,
            max_attempts: spec.max_attempts.unwrap_or(1).max(1),
            budget: spec.budget.clone().unwrap_or_default().or(&agent_budget),
            usage: Usage::default(),
            result: None,
            error: None,
            outputs: Vec::new(),
            tool_calls: Vec::new(),
            rules: Vec::new(),
            rules_held_back: 0,
            brief_tokens: 0,
            model: String::new(),
            created_ms: now_ms(),
            started_ms: None,
            finished_ms: None,
            run_ms: 0,
            pid: None,
        };
        let parent_rec = {
            let mut st = self.state.lock().unwrap();
            let (done, _) = watch::channel(false);
            st.tasks.insert(
                id.clone(),
                TaskRt {
                    record: record.clone(),
                    cancel: CancelToken::new(),
                    stop: None,
                    done,
                    slots: Default::default(),
                    waiting_for: Vec::new(),
                    delegations: 0,
                },
            );
            parent.and_then(|p| {
                st.tasks.get_mut(p).map(|t| {
                    t.record.children.push(id.clone());
                    t.record.clone()
                })
            })
        };
        if let Some(p) = parent_rec {
            self.save_task(&p);
        }
        self.save_task(&record);
        self.emit(EventKind::TaskCreated {
            task: id.clone(),
            owner: owner.to_string(),
            origin,
            parent: parent.map(str::to_string),
            objective: clip(&spec.objective, 300),
            after: spec.after,
        });
        Ok(id)
    }

    fn start(&self, id: &str) {
        let me = self.arc();
        let id = id.to_string();
        tokio::spawn(async move {
            let f = me.drive(&id).await;
            me.finish(&id, f);
        });
    }

    pub(crate) async fn wait_final(&self, id: &str) -> TaskRecord {
        let rx = {
            let st = self.state.lock().unwrap();
            st.tasks.get(id).map(|t| t.done.subscribe())
        };
        if let Some(mut rx) = rx {
            let _ = rx.wait_for(|d| *d).await;
        }
        let st = self.state.lock().unwrap();
        st.tasks
            .get(id)
            .map(|t| t.record.clone())
            .expect("a waited task stays known")
    }

    fn stop(&self, id: &str, stop: Stop) {
        let token = {
            let mut st = self.state.lock().unwrap();
            let Some(t) = st.tasks.get_mut(id) else {
                return;
            };
            if t.record.state.is_final() {
                return;
            }
            // A cancel overrides a suspend or a limit; nothing overrides a cancel.
            match (&t.stop, &stop) {
                (Some(Stop::Cancel), _) => {}
                (None, _) | (_, Stop::Cancel) => t.stop = Some(stop),
                _ => {}
            }
            t.cancel.clone()
        };
        token.cancel();
    }

    /// Stop a task and every open task below it.
    fn cancel_tree(&self, id: &str, stop: Stop) {
        let mut todo = vec![id.to_string()];
        while let Some(t) = todo.pop() {
            let children = {
                let st = self.state.lock().unwrap();
                st.tasks
                    .get(&t)
                    .map(|x| x.record.children.clone())
                    .unwrap_or_default()
            };
            self.stop(&t, stop.clone());
            todo.extend(children);
        }
    }

    fn retry(&self, id: &str) -> Result<(), OrchError> {
        {
            let mut st = self.state.lock().unwrap();
            let t = st
                .tasks
                .get_mut(id)
                .ok_or_else(|| OrchError::UnknownTask(id.into()))?;
            if !matches!(t.record.state, TaskState::Failed | TaskState::Cancelled) {
                return Err(OrchError::NotAllowed(format!(
                    "task {id} is {}; only a failed or cancelled task runs again",
                    t.record.state.as_str()
                )));
            }
            t.record.state = TaskState::Queued;
            t.record.error = None;
            t.record.finished_ms = None;
            t.record.max_attempts = t.record.attempts + 1;
            t.stop = None;
            t.cancel = CancelToken::new();
            t.done.send_replace(false);
        }
        let rec = self.update_task(id, |_| {});
        if let Some(r) = rec {
            self.emit(EventKind::TaskQueued {
                task: r.id,
                reason: "retry".into(),
            });
        }
        self.start(id);
        Ok(())
    }

    fn stop_reason(&self, id: &str) -> Option<Stop> {
        let st = self.state.lock().unwrap();
        st.tasks.get(id).and_then(|t| t.stop.clone())
    }

    fn cancel_token(&self, id: &str) -> CancelToken {
        let st = self.state.lock().unwrap();
        st.tasks
            .get(id)
            .map(|t| t.cancel.clone())
            .unwrap_or_default()
    }

    fn queued(&self, id: &str, state: TaskState, reason: String) {
        let changed = {
            let st = self.state.lock().unwrap();
            st.tasks.get(id).is_some_and(|t| {
                t.record.waiting_on.as_deref() != Some(&reason) || t.record.state != state
            })
        };
        if changed {
            self.update_task(id, |t| {
                t.state = state;
                t.waiting_on = Some(reason.clone());
            });
            self.emit(EventKind::TaskQueued {
                task: id.to_string(),
                reason,
            });
        }
    }

    /// Run a task to its end: dependencies, queueing, attempts, suspends.
    async fn drive(self: &Arc<Self>, id: &str) -> Final {
        let (owner, after) = {
            let st = self.state.lock().unwrap();
            let t = &st.tasks[id].record;
            (t.owner.clone(), t.after.clone())
        };
        if !after.is_empty() {
            self.queued(
                id,
                TaskState::Pending,
                format!("tasks {}", after.join(", ")),
            );
            for d in &after {
                let cancel = self.cancel_token(id);
                let rec = tokio::select! {
                    r = self.wait_final(d) => r,
                    _ = cancel.cancelled() => return self.stopped_before_start(id),
                };
                if rec.state != TaskState::Done {
                    return Final::failed(format!(
                        "task {d}, which this task runs after, is {}",
                        rec.state.as_str()
                    ));
                }
            }
        }
        let mut resumed = false;
        loop {
            if let Some(Stop::Cancel) = self.stop_reason(id) {
                return Final::cancelled("cancelled");
            }
            // The agent must take work.
            let (lock, mut status) = {
                let st = self.state.lock().unwrap();
                let a = &st.agents[&owner];
                (a.lock.clone(), a.status.subscribe())
            };
            let state = *status.borrow();
            match state {
                AgentState::Terminated => return Final::cancelled("the agent was terminated"),
                AgentState::Suspended => {
                    self.queued(
                        id,
                        TaskState::Suspended,
                        format!("agent {owner} is suspended"),
                    );
                    // Wake on resume, and now and then to notice a cancel.
                    tokio::select! {
                        _ = status.wait_for(|s| *s != AgentState::Suspended) => {}
                        _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                    }
                    continue;
                }
                _ => {
                    // Resumed after a suspend: run again with a fresh token.
                    if self.clear_suspend(id) {
                        resumed = true;
                    }
                }
            }
            // One task at a time per agent.
            let cancel = self.cancel_token(id);
            let guard = match lock.clone().try_lock_owned() {
                Ok(g) => g,
                Err(_) => {
                    self.queued(id, TaskState::Queued, format!("agent {owner} is busy"));
                    tokio::select! {
                        g = lock.clone().lock_owned() => g,
                        _ = cancel.cancelled() => return self.stopped_before_start(id),
                    }
                }
            };
            if matches!(
                self.agent_state(&owner),
                Some(AgentState::Suspended | AgentState::Terminated)
            ) {
                drop(guard);
                continue;
            }
            let models = match self.models_for(&owner).await {
                Ok(m) => m,
                Err(e) => return Final::failed(e.to_string()),
            };
            if !self.acquire(id, &models.account).await {
                return self.stopped_before_start(id);
            }
            let attempt = self.attempt(id, &owner, &models, resumed).await;
            self.release(id).await;
            drop(guard);
            match attempt {
                Attempt::Done(answer) => return Final::done(answer),
                Attempt::Stopped(Stop::Suspend) => {
                    self.update_task(id, |t| t.state = TaskState::Suspended);
                    continue;
                }
                Attempt::Stopped(Stop::Cancel) => return Final::cancelled("cancelled"),
                Attempt::Stopped(Stop::Limit(l)) => {
                    return Final::failed(format!("budget exceeded: {l}"))
                }
                Attempt::Failed(e) => {
                    let (attempts, max) = {
                        let st = self.state.lock().unwrap();
                        let t = &st.tasks[id].record;
                        (t.attempts, t.max_attempts)
                    };
                    if attempts >= max {
                        return Final::failed(e);
                    }
                    let delay = Duration::from_millis(500 * (1 << attempts.min(6)));
                    self.emit(EventKind::TaskRetry {
                        task: id.to_string(),
                        attempt: attempts + 1,
                        error: clip(&e, 300),
                        delay_ms: delay.as_millis() as u64,
                    });
                    let cancel = self.cancel_token(id);
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = cancel.cancelled() => return self.stopped_before_start(id),
                    }
                }
            }
        }
    }

    fn agent_state(&self, agent: &str) -> Option<AgentState> {
        let st = self.state.lock().unwrap();
        st.agents.get(agent).map(|a| a.record.state)
    }

    /// After a suspend the task needs a fresh token to run again. Returns whether it was
    /// suspended.
    fn clear_suspend(&self, id: &str) -> bool {
        let mut st = self.state.lock().unwrap();
        match st.tasks.get_mut(id) {
            Some(t) if t.stop == Some(Stop::Suspend) => {
                t.stop = None;
                t.cancel = CancelToken::new();
                true
            }
            _ => false,
        }
    }

    fn stopped_before_start(&self, id: &str) -> Final {
        match self.stop_reason(id) {
            Some(Stop::Limit(l)) => Final::failed(format!("budget exceeded: {l}")),
            _ => Final::cancelled("cancelled"),
        }
    }

    fn account_slots(&self, account: &str) -> Arc<Semaphore> {
        self.accounts
            .lock()
            .unwrap()
            .entry(account.to_string())
            .or_insert_with(|| {
                Arc::new(Semaphore::new(
                    self.models.capacity_of(account).max_concurrent.max(1),
                ))
            })
            .clone()
    }

    /// Take a slot on the account and a global one, queueing (visibly) while there is none.
    /// False: cancelled while waiting.
    async fn acquire(&self, id: &str, account: &str) -> bool {
        let slots = {
            let st = self.state.lock().unwrap();
            st.tasks[id].slots.clone()
        };
        let mut s = slots.lock().await;
        let cancel = self.cancel_token(id);
        let sem = self.account_slots(account);
        let permit = match sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                let max = self.models.capacity_of(account).max_concurrent.max(1);
                self.queued(
                    id,
                    TaskState::Queued,
                    format!("capacity: {account} has {max} of {max} slots in use"),
                );
                tokio::select! {
                    p = sem.clone().acquire_owned() => p.expect("never closed"),
                    _ = cancel.cancelled() => return false,
                }
            }
        };
        let global = match self.global.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                self.queued(
                    id,
                    TaskState::Queued,
                    format!("capacity: {} tasks running, the limit", self.max_parallel),
                );
                tokio::select! {
                    p = self.global.clone().acquire_owned() => p.expect("never closed"),
                    _ = cancel.cancelled() => return false,
                }
            }
        };
        s.account = Some(permit);
        s.global = Some(global);
        s.account_name = account.to_string();
        true
    }

    async fn release(&self, id: &str) {
        let slots = {
            let mut st = self.state.lock().unwrap();
            let t = st.tasks.get_mut(id).expect("a running task exists");
            // A wait cut short by a stop leaves nothing to wait for.
            t.waiting_for.clear();
            t.slots.clone()
        };
        let mut s = slots.lock().await;
        s.account = None;
        s.global = None;
        s.waiters = 0;
    }

    /// The rules for a task: the task's sets first, then the agent's, then the project's.
    fn rules_for(&self, task: &TaskRecord) -> Vec<ark_core::RuleSet> {
        RuleBook::new(&self.engine.store)
            .resolve(&[
                RuleScope::Task(task.id.clone()),
                RuleScope::Agent(task.owner.clone()),
                RuleScope::Project(self.project.id.to_string()),
            ])
            .unwrap_or_else(|e| {
                tracing::warn!(task = %task.id, "could not read rule sets: {e}");
                Vec::new()
            })
    }

    fn agent_profile(&self, spec: &AgentSpec, delegates: &[(String, String)]) -> AgentProfile {
        let named = spec
            .profile_name
            .as_ref()
            .and_then(|n| self.profiles.iter().find(|p| &p.name == n));
        let mut p = match (&spec.profile, named) {
            (Some(p), _) => p.clone(),
            (None, Some(n)) => n.profile.clone(),
            (None, None) => self.base.clone(),
        };
        if !spec.instructions.trim().is_empty() {
            p.prompts.instructions = spec.instructions.clone();
        }
        let mut who = format!("\n\nYou are the agent \"{}\"", spec.name);
        if !spec.role.trim().is_empty() {
            who.push_str(&format!(" ({})", spec.role.trim()));
        }
        who.push_str(
            ". Each request is one task; its first message says what it is and who asked.",
        );
        if !delegates.is_empty() {
            who.push_str("\nYou can delegate with agents.delegate to:\n");
            for (n, r) in delegates {
                who.push_str(&format!("- {n}: {}\n", if r.is_empty() { "-" } else { r }));
            }
            who.push_str(
                "An agent you delegate to sees only the objective, context and artifacts you give it, not this conversation. \
Delegate independent pieces in the same reply so they run in parallel, then combine their results.",
            );
        }
        p.prompts.instructions.push_str(&who);
        let mut preload: Vec<&str> = Vec::new();
        if !delegates.is_empty() {
            preload.extend([tools::DELEGATE, tools::WAIT]);
        }
        // An agent asked to draw should not spend a step discovering how.
        if spec.diagrams {
            preload.push(ark_tools::diagram::CREATE);
        }
        for a in preload {
            if !p.core_actions.iter().any(|c| c == a) {
                p.core_actions.push(a.to_string());
            }
        }
        p.distillers.push(tools::distiller());
        p
    }

    /// One harness turn for the task in the agent's project.
    async fn attempt(
        self: &Arc<Self>,
        id: &str,
        owner: &str,
        models: &AgentModels,
        resumed: bool,
    ) -> Attempt {
        let (task, agent, deps, children, inbox, cancel, roster) = {
            let mut st = self.state.lock().unwrap();
            let task = st.tasks[id].record.clone();
            let cancel = st.tasks[id].cancel.clone();
            let deps: Vec<TaskRecord> = task
                .after
                .iter()
                .filter_map(|d| st.tasks.get(d).map(|t| t.record.clone()))
                .collect();
            let children: Vec<TaskRecord> = task
                .children
                .iter()
                .filter_map(|c| st.tasks.get(c).map(|t| t.record.clone()))
                .collect();
            let roster: Vec<(String, String, AgentState)> = st
                .agents
                .values()
                .map(|a| {
                    (
                        a.record.name.clone(),
                        a.record.spec.role.clone(),
                        a.record.state,
                    )
                })
                .collect();
            let a = st.agents.get_mut(owner).expect("the owner exists");
            let inbox = std::mem::take(&mut a.record.inbox);
            (
                task,
                a.record.clone(),
                deps,
                children,
                inbox,
                cancel,
                roster,
            )
        };
        let spec = &agent.spec;
        let mut delegates: Vec<(String, String)> = roster
            .iter()
            .filter(|(n, _, s)| spec.may_delegate(n) && *s != AgentState::Terminated)
            .map(|(n, r, _)| (n.clone(), r.clone()))
            .collect();
        delegates.sort();
        let mut contacts: Vec<String> = roster
            .iter()
            .filter(|(n, _, s)| spec.may_contact(n) && *s != AgentState::Terminated)
            .map(|(n, _, _)| n.clone())
            .collect();
        contacts.sort();

        // Its tools: the allowed app actions, its agent tools, diagrams, rules.
        let mut registry: Registry = self
            .engine
            .registry
            .filtered(|a| spec.tools.iter().any(|p| action_matches(p, a)));
        if !delegates.is_empty() || !contacts.is_empty() {
            registry.register(Arc::new(AgentTools {
                orch: self.me.clone(),
                agent: owner.to_string(),
                task: id.to_string(),
                delegates: delegates.clone(),
                contacts,
            }));
        }
        if spec.diagrams {
            registry.register(Arc::new(ark_tools::diagram::Diagrams));
        }
        let sets = self.rules_for(&task);
        let objective = format!("{}\n{}", task.objective, task.context);
        let selection = (!sets.is_empty()).then(|| select(&sets, &objective, RULES_BUDGET));
        if !sets.is_empty() {
            registry.register(Arc::new(RulesTool::new(sets)));
        }
        let engine = self.engine.scoped(registry);
        let profile = self.agent_profile(spec, &delegates);
        let scope = ProjectId(agent.scope.clone());

        let dep_refs: Vec<&TaskRecord> = deps.iter().collect();
        let brief = Brief {
            task: &task,
            deps: dep_refs,
            children: children.iter().collect(),
            inbox: &inbox,
            resumed,
        }
        .render();
        let brief_tokens = estimate_tokens(&brief);

        // Its conversation: carry-over between its own tasks, never anyone else's.
        let mut session = match agent
            .session
            .as_deref()
            .and_then(|s| Session::open(&self.engine, s).ok().flatten())
        {
            Some(s) => s,
            None => match Session::start(&self.engine, &scope) {
                Ok(s) => s,
                Err(e) => return Attempt::Failed(e.to_string()),
            },
        };
        let steer = Steering::new();
        let budget = &task.budget;
        let harness = Harness::new(models.router.clone_view())
            .max_steps(budget.max_steps.unwrap_or(self.max_steps))
            .budget(self.context_budget)
            .planner(spec.planner.unwrap_or(self.planner))
            .critic(spec.critic.unwrap_or(self.critic))
            .allow_mutate(spec.allow_mutate)
            .approvals(match &self.approvals {
                Some(gate) if !spec.allow_mutate => {
                    Some(Arc::new(ApprovalGate::new(Arc::new(OnBehalf {
                        agent: owner.to_string(),
                        task: id.to_string(),
                        gate: gate.clone(),
                    }))))
                }
                _ => None,
            })
            .cancel(Some(cancel.clone()))
            .steering(Some(steer.clone()))
            .conversation(session.conversation_digest())
            .rules(selection.clone());

        let session_id = session.id().to_string();
        let attempt_no = {
            let rec = self.update_task(id, |t| {
                t.state = TaskState::Running;
                t.waiting_on = None;
                t.attempts += 1;
                t.started_ms.get_or_insert(now_ms());
                t.pid = Some(std::process::id());
                t.model = models.model.clone();
                t.brief_tokens = brief_tokens;
                if let Some(s) = &selection {
                    t.rules = s.applied.clone();
                    t.rules_held_back = s.held_back;
                }
            });
            rec.map(|r| r.attempts).unwrap_or(1)
        };
        {
            let mut st = self.state.lock().unwrap();
            if let Some(a) = st.agents.get_mut(owner) {
                a.steering = Some(steer.clone());
            }
        }
        self.update_agent(owner, |a| {
            a.current_task = Some(id.to_string());
            a.session = Some(session_id.clone());
        });
        self.set_agent_state(owner, AgentState::Running);
        self.emit(EventKind::TaskStarted {
            task: id.to_string(),
            agent: owner.to_string(),
            attempt: attempt_no,
            model: models.model.clone(),
            brief_tokens,
        });
        if let Some(s) = selection.as_ref().filter(|s| !s.is_empty()) {
            self.emit(EventKind::RulesApplied {
                task: id.to_string(),
                applied: s.applied.clone(),
                held_back: s.held_back,
            });
        }

        // A time limit trips the token, so the harness stops cleanly and saves its state.
        let timer = budget.timeout_secs.map(|secs| {
            let me = self.clone();
            let id = id.to_string();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                me.limit(&id, format!("time: {secs}s"));
            })
        });
        if let Err(e) = session.begin_turn(&self.engine, &brief) {
            tracing::warn!(task = %id, "could not record the turn start: {e}");
        }
        let started = std::time::Instant::now();
        let me = self.clone();
        let (tid, aname) = (id.to_string(), owner.to_string());
        let result = harness
            .ask(&engine, &profile, &scope, &brief, move |ev| {
                me.on_agent_event(&tid, &aname, ev)
            })
            .await;
        if let Some(t) = timer {
            t.abort();
        }
        let ran = started.elapsed().as_millis() as u64;
        self.update_task(id, |t| t.run_ms += ran);
        {
            let mut st = self.state.lock().unwrap();
            if let Some(a) = st.agents.get_mut(owner) {
                a.steering = None;
                // Messages that arrived too late for this task wait for the next one.
                for text in steer.take_undelivered() {
                    a.record.inbox.push(Mail {
                        from: "?".into(),
                        text,
                        task: None,
                        at_ms: now_ms(),
                    });
                }
            }
        }
        self.collect_outputs(id, owner, &scope);
        match result {
            Ok(outcome) if !outcome.cancelled => {
                if let Err(e) = session.record_turn(&self.engine, &brief, &outcome) {
                    tracing::warn!(task = %id, "could not record the turn: {e}");
                }
                Attempt::Done(outcome.answer)
            }
            Ok(outcome) => {
                let _ = session.record_turn(&self.engine, &brief, &outcome);
                Attempt::Stopped(self.stop_reason(id).unwrap_or(Stop::Cancel))
            }
            Err(e) => {
                let _ = session.fail_turn(&self.engine, &brief, &e.to_string());
                match self.stop_reason(id) {
                    Some(s) => Attempt::Stopped(s),
                    None => Attempt::Failed(e.to_string()),
                }
            }
        }
    }

    fn limit(&self, id: &str, what: String) {
        let open = {
            let st = self.state.lock().unwrap();
            st.tasks
                .get(id)
                .is_some_and(|t| !t.record.state.is_final() && t.stop.is_none())
        };
        if open {
            self.emit(EventKind::BudgetExceeded {
                task: id.to_string(),
                limit: what.clone(),
            });
            self.cancel_tree(id, Stop::Limit(what));
        }
    }

    /// Record what the agent did, check the budget, and pass the event on.
    fn on_agent_event(&self, task: &str, agent: &str, ev: HarnessEvent) {
        let mut over = None;
        let mut save = None;
        {
            let mut st = self.state.lock().unwrap();
            if let Some(t) = st.tasks.get_mut(task) {
                let r = &mut t.record;
                match &ev {
                    HarnessEvent::Step { .. } => r.usage.steps += 1,
                    HarnessEvent::ModelCall { usage, .. } => {
                        r.usage.prompt_tokens += usage.prompt_tokens as u64;
                        r.usage.completion_tokens += usage.completion_tokens as u64;
                        r.usage.cached_tokens += usage.cached_tokens as u64;
                        r.usage.model_calls += 1;
                        r.usage.measured &= usage.measured;
                        if let Some(max) = r.budget.max_tokens.filter(|m| r.usage.tokens() > *m) {
                            over = Some(format!("tokens: {} of {max}", r.usage.tokens()));
                        }
                        if let Some(max) = r
                            .budget
                            .max_model_calls
                            .filter(|m| r.usage.model_calls > *m)
                        {
                            over = Some(format!("model calls: {} of {max}", r.usage.model_calls));
                        }
                        save = Some(r.clone());
                    }
                    HarnessEvent::ToolCall { id, action, .. } => {
                        r.tool_calls.push(ToolCallRecord {
                            id: id.clone(),
                            action: action.clone(),
                            ok: None,
                            summary: String::new(),
                            invocation: String::new(),
                            duration_ms: None,
                            reused: false,
                        });
                        let n = r.tool_calls.len();
                        if n > TOOL_CALLS_KEPT {
                            r.tool_calls.drain(..n - TOOL_CALLS_KEPT);
                        }
                    }
                    HarnessEvent::ToolResult {
                        id,
                        ok,
                        summary,
                        invocation,
                        duration_ms,
                        ..
                    } => {
                        if let Some(c) = r.tool_calls.iter_mut().rev().find(|c| &c.id == id) {
                            c.ok = Some(*ok);
                            c.summary = clip(summary, 300);
                            c.invocation = invocation.clone();
                            c.duration_ms = *duration_ms;
                        }
                        r.usage.tool_calls += 1;
                        save = Some(r.clone());
                    }
                    HarnessEvent::Duplicate { action, reused } => {
                        r.tool_calls.push(ToolCallRecord {
                            id: String::new(),
                            action: action.clone(),
                            ok: Some(true),
                            summary: clip(reused, 300),
                            invocation: String::new(),
                            duration_ms: None,
                            reused: true,
                        });
                    }
                    _ => {}
                }
            }
            if let Some(a) = st.agents.get_mut(agent) {
                match &ev {
                    HarnessEvent::ModelCall { usage, .. } => {
                        a.record.usage.prompt_tokens += usage.prompt_tokens as u64;
                        a.record.usage.completion_tokens += usage.completion_tokens as u64;
                        a.record.usage.cached_tokens += usage.cached_tokens as u64;
                        a.record.usage.model_calls += 1;
                        a.record.usage.measured &= usage.measured;
                    }
                    HarnessEvent::Context { report } if report.role == "reason" => {
                        a.record.last_context = Some(report.clone());
                    }
                    HarnessEvent::ToolResult { .. } => a.record.usage.tool_calls += 1,
                    HarnessEvent::Step { .. } => a.record.usage.steps += 1,
                    _ => {}
                }
            }
        }
        if let Some(r) = save {
            self.save_task(&r);
        }
        self.emit(EventKind::Agent {
            task: task.to_string(),
            agent: agent.to_string(),
            event: ev,
        });
        if let Some(limit) = over {
            self.limit(task, limit);
        }
    }

    /// Artifacts the task's tool calls produced, added to the task's outputs.
    fn collect_outputs(&self, id: &str, agent: &str, scope: &ProjectId) {
        let (invocations, known): (Vec<String>, Vec<String>) = {
            let st = self.state.lock().unwrap();
            let t = &st.tasks[id].record;
            (
                t.tool_calls
                    .iter()
                    .filter(|c| !c.invocation.is_empty())
                    .map(|c| c.invocation.clone())
                    .collect(),
                t.outputs.iter().map(|o| o.id.clone()).collect(),
            )
        };
        let mut new = Vec::new();
        for inv in invocations {
            let Ok(Some(inv)) = self.engine.store.get_invocation(scope, &inv) else {
                continue;
            };
            for a in inv.produced {
                if known.contains(&a.0) || new.iter().any(|x: &ArtifactRef| x.id == a.0) {
                    continue;
                }
                if let Ok(Some(art)) = self.engine.store.resolve_artifact(scope, a.as_str()) {
                    new.push(ArtifactRef {
                        id: art.id.to_string(),
                        name: art.name.clone(),
                        kind: art.kind.as_tag(),
                    });
                }
            }
        }
        if new.is_empty() {
            return;
        }
        self.update_task(id, |t| t.outputs.extend(new.clone()));
        for a in new {
            self.emit(EventKind::ArtifactProduced {
                task: id.to_string(),
                agent: agent.to_string(),
                artifact: a,
            });
        }
    }

    fn finish(&self, id: &str, f: Final) {
        let rec = self.update_task(id, |t| {
            t.state = f.state;
            t.result = f.result.clone();
            t.error = f.error.clone();
            t.waiting_on = None;
            t.finished_ms = Some(now_ms());
            t.pid = None;
        });
        let Some(rec) = rec else { return };
        let owner = rec.owner.clone();
        let current = self
            .state
            .lock()
            .unwrap()
            .agents
            .get(&owner)
            .and_then(|a| a.record.current_task.clone());
        self.update_agent(&owner, |a| {
            if current.as_deref() == Some(id) {
                a.current_task = None;
            }
            match f.state {
                TaskState::Done => a.tasks_done += 1,
                TaskState::Failed => a.tasks_failed += 1,
                _ => {}
            }
        });
        if current.as_deref() == Some(id) && self.agent_state(&owner) == Some(AgentState::Running) {
            self.set_agent_state(&owner, AgentState::Idle);
        }
        // Results go back up: the parent's agent can use what this task produced.
        if let (Some(parent), true) = (&rec.parent, !rec.outputs.is_empty()) {
            let parent_owner = {
                let st = self.state.lock().unwrap();
                st.tasks.get(parent).map(|t| t.record.owner.clone())
            };
            if let (Some(po), Some(from)) = (parent_owner, self.scope_of(&owner)) {
                if let Some(to) = self.scope_of(&po) {
                    let ids: Vec<String> = rec.outputs.iter().map(|o| o.id.clone()).collect();
                    if let Err(e) = self.share_artifacts(&from, &to, &ids) {
                        tracing::warn!(task = %id, "could not pass artifacts up: {e}");
                    }
                }
            }
        }
        // Nobody is waiting for a task delegated without `wait` whose parent has ended: tell
        // the parent's agent in its next brief.
        if let Origin::Agent { agent, task } = &rec.origin {
            let parent_open = {
                let st = self.state.lock().unwrap();
                st.tasks
                    .get(task)
                    .is_some_and(|t| !t.record.state.is_final())
            };
            if !parent_open {
                let text = format!(
                    "Task {} ({}) is {}: {}",
                    rec.id,
                    rec.owner,
                    rec.state.as_str(),
                    clip(
                        rec.result.as_deref().or(rec.error.as_deref()).unwrap_or(""),
                        600
                    )
                );
                self.update_agent(agent, |a| {
                    a.inbox.push(Mail {
                        from: rec.owner.clone(),
                        text,
                        task: Some(rec.id.clone()),
                        at_ms: now_ms(),
                    })
                });
            }
        }
        if f.state != TaskState::Done {
            for c in &rec.children {
                self.cancel_tree(c, Stop::Cancel);
            }
        }
        self.emit(EventKind::TaskFinished {
            task: id.to_string(),
            state: rec.state,
            result: rec.result.as_ref().map(|r| clip(r, 2000)),
            error: rec.error.clone(),
            usage: rec.usage,
            run_ms: rec.run_ms,
        });
        let st = self.state.lock().unwrap();
        if let Some(t) = st.tasks.get(id) {
            t.done.send_replace(true);
        }
    }

    // ---- delegation and messages ---------------------------------------------------

    pub(crate) fn is_child(&self, parent: &str, task: &str) -> bool {
        let st = self.state.lock().unwrap();
        st.tasks
            .get(task)
            .is_some_and(|t| t.record.parent.as_deref() == Some(parent))
    }

    pub(crate) fn delegate(
        &self,
        from_agent: &str,
        from_task: &str,
        to: &str,
        spec: TaskSpec,
    ) -> Result<String, OrchError> {
        {
            let mut st = self.state.lock().unwrap();
            let from = st
                .agents
                .get(from_agent)
                .ok_or_else(|| OrchError::UnknownAgent(from_agent.into()))?;
            if !from.record.spec.may_delegate(to) {
                return Err(OrchError::NotAllowed(format!(
                    "{from_agent} may not delegate to {to}"
                )));
            }
            if !st.agents.contains_key(to) {
                return Err(OrchError::UnknownAgent(to.into()));
            }
            // An agent up this chain is busy waiting for it: delegating to it would wait on
            // itself.
            let mut cur = Some(from_task.to_string());
            while let Some(t) = cur {
                let Some(rt) = st.tasks.get(&t) else { break };
                if rt.record.owner == to {
                    return Err(OrchError::NotAllowed(format!(
                        "{to} is already working on {t}, which waits for this task; delegating to it would never finish"
                    )));
                }
                cur = rt.record.parent.clone();
            }
            let t = st
                .tasks
                .get_mut(from_task)
                .ok_or_else(|| OrchError::UnknownTask(from_task.into()))?;
            if let Some(max) = t.record.budget.max_delegations {
                if t.delegations >= max {
                    return Err(OrchError::NotAllowed(format!(
                        "the task may delegate {max} times; it has"
                    )));
                }
            }
            t.delegations += 1;
        }
        let id = self.create_task(
            to,
            spec,
            Origin::Agent {
                agent: from_agent.to_string(),
                task: from_task.to_string(),
            },
            Some(from_task),
        )?;
        self.emit(EventKind::Delegated {
            from_agent: from_agent.to_string(),
            from_task: from_task.to_string(),
            to_agent: to.to_string(),
            task: id.clone(),
        });
        self.start(&id);
        Ok(id)
    }

    /// Wait for tasks `parent` delegated, giving back its capacity meanwhile.
    pub(crate) async fn wait_children(
        this: &Arc<Inner>,
        parent: &str,
        ids: &[String],
    ) -> Vec<TaskRecord> {
        this.park(parent, ids).await;
        let mut out = Vec::new();
        for id in ids {
            out.push(this.wait_final(id).await);
        }
        this.unpark(parent, ids).await;
        out
    }

    async fn park(&self, parent: &str, ids: &[String]) {
        let (slots, owner) = {
            let mut st = self.state.lock().unwrap();
            let t = st.tasks.get_mut(parent).expect("a parent task exists");
            t.waiting_for.extend(ids.iter().cloned());
            (t.slots.clone(), t.record.owner.clone())
        };
        let mut s = slots.lock().await;
        s.waiters += 1;
        if s.waiters == 1 {
            s.account = None;
            s.global = None;
        }
        let on = {
            let st = self.state.lock().unwrap();
            st.tasks[parent].waiting_for.clone()
        };
        self.update_task(parent, |t| {
            t.state = TaskState::Waiting;
            t.waiting_on = Some(format!("tasks {}", on.join(", ")));
        });
        self.set_agent_state(&owner, AgentState::Waiting);
        self.emit(EventKind::TaskWaiting {
            task: parent.to_string(),
            on: ids.to_vec(),
        });
    }

    async fn unpark(&self, parent: &str, ids: &[String]) {
        let (slots, owner, cancel) = {
            let mut st = self.state.lock().unwrap();
            let t = st.tasks.get_mut(parent).expect("a parent task exists");
            t.waiting_for.retain(|w| !ids.contains(w));
            (t.slots.clone(), t.record.owner.clone(), t.cancel.clone())
        };
        let mut s = slots.lock().await;
        s.waiters = s.waiters.saturating_sub(1);
        if s.waiters > 0 {
            let on = {
                let st = self.state.lock().unwrap();
                st.tasks[parent].waiting_for.clone()
            };
            self.update_task(parent, |t| {
                t.waiting_on = Some(format!("tasks {}", on.join(", ")))
            });
            return;
        }
        if cancel.is_cancelled() {
            return;
        }
        let account = s.account_name.clone();
        let sem = self.account_slots(&account);
        let permit = match sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                let max = self.models.capacity_of(&account).max_concurrent.max(1);
                self.queued(
                    parent,
                    TaskState::Queued,
                    format!("capacity: {account} has {max} of {max} slots in use"),
                );
                tokio::select! {
                    p = sem.clone().acquire_owned() => p.expect("never closed"),
                    _ = cancel.cancelled() => return,
                }
            }
        };
        let global = tokio::select! {
            p = self.global.clone().acquire_owned() => p.expect("never closed"),
            _ = cancel.cancelled() => return,
        };
        s.account = Some(permit);
        s.global = Some(global);
        self.update_task(parent, |t| {
            t.state = TaskState::Running;
            t.waiting_on = None;
        });
        self.set_agent_state(&owner, AgentState::Running);
        self.emit(EventKind::TaskResumed {
            task: parent.to_string(),
        });
    }

    /// Deliver a message into `to`'s running task, or keep it for its next one. Returns
    /// whether it went into a running task.
    pub(crate) fn send(
        &self,
        from: &str,
        to: &str,
        text: &str,
        task: Option<&str>,
    ) -> Result<bool, OrchError> {
        if text.trim().is_empty() {
            return Err(OrchError::NotAllowed("an empty message".into()));
        }
        let delivered = {
            let mut st = self.state.lock().unwrap();
            let a = st
                .agents
                .get_mut(to)
                .ok_or_else(|| OrchError::UnknownAgent(to.into()))?;
            if a.record.state == AgentState::Terminated {
                return Err(OrchError::Terminated(to.into()));
            }
            match &a.steering {
                Some(s) => {
                    s.push(format!("Message from {from}: {text}"));
                    true
                }
                None => {
                    a.record.inbox.push(Mail {
                        from: from.to_string(),
                        text: text.to_string(),
                        task: task.map(str::to_string),
                        at_ms: now_ms(),
                    });
                    false
                }
            }
        };
        if !delivered {
            self.update_agent(to, |_| {});
        }
        let edge = crate::graph::MessageEdge {
            from: from.to_string(),
            to: to.to_string(),
            task: task.map(str::to_string),
            at_ms: now_ms(),
        };
        let _ = serde_json::to_value(&edge).map(|v| {
            let id = format!("{}-{}", edge.at_ms, uuid::Uuid::new_v4().simple());
            self.engine
                .store
                .put_record(MESSAGE, &id, self.project.id.as_str(), &v)
        });
        self.state.lock().unwrap().messages.push(edge);
        self.emit(EventKind::Message {
            from: from.to_string(),
            to: to.to_string(),
            task: task.map(str::to_string),
            text: clip(text, 1000),
            delivered,
        });
        Ok(delivered)
    }
}

/// Asks the orchestrator's gate on behalf of one agent's task, so the person sees who asks.
/// Grants are kept by the orchestrator's gate.
struct OnBehalf {
    agent: String,
    task: String,
    gate: Arc<ApprovalGate>,
}

#[async_trait::async_trait]
impl Approver for OnBehalf {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let mut r = request.clone();
        r.agent = Some(self.agent.clone());
        r.task = Some(self.task.clone());
        self.gate.check(&r).await
    }
}

/// Why a task left running by a stopped process failed.
pub const INTERRUPTED: &str = "interrupted: the process stopped while it ran";

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists; nothing is delivered.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    false
}
