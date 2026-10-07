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
use ark_runtime::skills;
use ark_runtime::{
    AgentProfile, ApprovalDecision, ApprovalGate, ApprovalRequest, Approver, CancelToken, Engine,
    Harness, HarnessEvent, Post, RuleBook, RulesTool, Session, Steering,
};
use ark_sdk::Ark;
use ark_tools::{action_matches, Registry};
use tokio::sync::{broadcast, watch, OwnedSemaphorePermit, Semaphore};

use crate::board::{self, rank_before, WorkChange, WorkItem, WorkState, PERSON, WORK};
use crate::brief::{clip, Brief};
use crate::event::{EventKind, OrchEvent};
use crate::models::{AgentModels, ModelResolver, Models, ModelsBuilder};
use crate::record::{
    now_ms, AgentRecord, AgentState, ArtifactRef, Mail, Origin, Question, Step, TaskRecord,
    TaskState, ToolCallRecord, Usage, TOOL_CALLS_KEPT,
};
use crate::schedule::{ScheduleRecord, When, SCHEDULE};
use crate::spec::{AgentSpec, TaskSpec};
use crate::tools::{self, AgentTools};
use crate::OrchError;

mod elsewhere;

const AGENT: &str = "orch_agent";
pub(crate) const TASK: &str = "orch_task";
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
                // Agents without a model of their own follow the app's model as it changes.
                let mut b = Models::builder()
                    .follow(self.ark.current_model())
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
            per_agent: self.ark.per_agent().to_vec(),
            workers: self.ark.workers().cloned(),
            folders: Mutex::new(HashMap::new()),
            max_parallel: self.max_parallel,
            global: Arc::new(Semaphore::new(self.max_parallel)),
            accounts: Mutex::new(HashMap::new()),
            chat_waits: Mutex::new(Vec::new()),
            state: Mutex::new(State::default()),
            events,
            ticks: Mutex::new(()),
        });
        let recover = inner.load()?;
        let orch = Orchestrator { inner };
        // Schedules fire whether or not anything else runs: no window needs to be open. Work a
        // stopped process left open continues the same way.
        if tokio::runtime::Handle::try_current().is_ok() {
            orch.spawn_schedule_timer(SCHEDULE_TICK);
            orch.inner.recover(&recover);
        }
        Ok(orch)
    }
}

/// How often the timer looks for schedules that are due.
const SCHEDULE_TICK: Duration = Duration::from_secs(20);

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

    /// The tools of the person's own conversation: create an agent, hand one a task, wait
    /// for tasks (see [`ChatTools`](crate::tools)). Add them to each turn with
    /// [`TurnOptions::tools`](ark_sdk::TurnOptions::tools); they name the agents of that moment.
    pub fn chat_tools(&self) -> ark_sdk::TurnTools {
        self.chat_tools_in(None)
    }

    /// [`chat_tools`](Self::chat_tools) for one conversation (`session`): what it schedules
    /// posts each run's result into it ([`TaskSpec::report_to`]).
    pub fn chat_tools_for(&self, session: &str) -> ark_sdk::TurnTools {
        self.chat_tools_in(Some(session.to_string()))
    }

    fn chat_tools_in(&self, chat: Option<String>) -> ark_sdk::TurnTools {
        ark_sdk::TurnTools {
            adapter: Arc::new(tools::ChatTools {
                orch: self.clone(),
                chat,
            }),
            distiller: Some(tools::distiller()),
        }
    }

    /// The project's board for the person's conversation: `board.list`, `board.add`,
    /// `board.update` (see [`crate::board`]). Add them beside [`chat_tools`](Self::chat_tools).
    pub fn board_tools(&self) -> ark_sdk::TurnTools {
        ark_sdk::TurnTools {
            adapter: Arc::new(tools::BoardTools { orch: self.clone() }),
            distiller: Some(tools::board_distiller()),
        }
    }

    /// The tool namespaces an agent can be given (`web`, `browser`, `mail`, the app's own), in
    /// the order the app offers them. What `agents.create` lists as capabilities.
    pub fn capabilities(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let listed = self
            .inner
            .engine
            .tools()
            .adapters()
            .iter()
            .map(|a| a.name().to_string())
            .chain(
                self.inner
                    .per_agent
                    .iter()
                    .map(|p| p.listing().name().to_string()),
            )
            .collect::<Vec<_>>();
        for n in listed {
            if !names.contains(&n) {
                names.push(n);
            }
        }
        names
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

    /// [`wait`](Self::wait) in a turn of `chat`: a task that reports to it gets its result there
    /// as this answer, not as a post too.
    pub(crate) async fn wait_in(
        &self,
        task: &str,
        chat: Option<&str>,
    ) -> Result<TaskRecord, OrchError> {
        let Some(chat) = chat else {
            return self.wait(task).await;
        };
        let w = self.inner.chat_wait(task, chat);
        let rec = self.wait(task).await?;
        w.took(&rec);
        Ok(rec)
    }

    pub fn task(&self, id: &str) -> Option<TaskRecord> {
        let st = self.inner.state.lock().unwrap();
        st.tasks.get(id).map(|t| t.record.clone())
    }

    /// Open tasks waiting for the person to answer their question, longest waiting first. Each
    /// task's `question` says what it asks (ADR 0036).
    pub fn questions(&self) -> Vec<TaskRecord> {
        let st = self.inner.state.lock().unwrap();
        let mut v: Vec<TaskRecord> = st
            .tasks
            .values()
            .map(|t| &t.record)
            .filter(|r| !r.state.is_final() && r.question.as_ref().is_some_and(Question::open))
            .cloned()
            .collect();
        v.sort_by_key(|r| r.question.as_ref().map(|q| q.asked_ms));
        v
    }

    /// Answer a task's question. While its run waits, the answer goes to it. When that run is
    /// gone (the process restarted), the answer is kept and the task runs again; when its agent
    /// asks the same call, this answers it. Grants the answer gives are kept either way.
    pub fn answer(&self, task: &str, decision: ApprovalDecision) -> Result<(), OrchError> {
        let inner = &self.inner;
        let (live, question, answered) = {
            let mut st = inner.state.lock().unwrap();
            let t = st
                .tasks
                .get_mut(task)
                .ok_or_else(|| OrchError::UnknownTask(task.into()))?;
            let q = t
                .record
                .question
                .clone()
                .filter(|q| q.open() && !t.record.state.is_final())
                .ok_or_else(|| OrchError::NotAllowed(format!("task {task} waits for no answer")))?;
            (t.answer.take(), q, t.answered.clone())
        };
        if let Some(tx) = live {
            if tx.send(decision).is_ok() {
                return Ok(());
            }
        }
        if let Some(gate) = &inner.approvals {
            gate.remember(&question.request, decision);
        }
        inner.update_task(task, |t| {
            if let Some(q) = &mut t.question {
                q.answer = Some(decision);
            }
        });
        inner.emit(EventKind::Answered {
            task: task.to_string(),
            question: question.id,
            decision,
        });
        answered.notify_one();
        Ok(())
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
        self.inner.terminate(agent).await?;
        // An ended agent runs nothing on a schedule either.
        let mine: Vec<String> = {
            let st = self.inner.state.lock().unwrap();
            st.schedules
                .values()
                .filter(|s| s.agent == agent)
                .map(|s| s.id.clone())
                .collect()
        };
        for id in mine {
            self.inner.drop_schedule(&id);
        }
        Ok(())
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

    // ---- schedules ---------------------------------------------------------------------

    /// Hand `agent` this task at the times `when` says, from now on, until
    /// [`unschedule`](Self::unschedule) (see [`crate::schedule`]). Each run is a task like any
    /// other; its state-changing actions ask, as the agent's always do.
    pub fn schedule(
        &self,
        agent: &str,
        task: TaskSpec,
        when: When,
    ) -> Result<ScheduleRecord, OrchError> {
        let when = when.normalized().map_err(OrchError::NotAllowed)?;
        match self.agent(agent) {
            None => return Err(OrchError::UnknownAgent(agent.into())),
            Some(a) if a.state == AgentState::Terminated => {
                return Err(OrchError::Terminated(agent.into()))
            }
            _ => {}
        }
        if task.objective.trim().is_empty() {
            return Err(OrchError::NotAllowed("a task needs an objective".into()));
        }
        let rec = ScheduleRecord::new(self.inner.project.id.as_str(), agent, task, when);
        self.inner.save_schedule(&rec);
        self.inner
            .state
            .lock()
            .unwrap()
            .schedules
            .insert(rec.id.clone(), rec.clone());
        self.inner.emit(EventKind::Scheduled {
            schedule: rec.id.clone(),
            agent: agent.to_string(),
            objective: clip(&rec.task.objective, 300),
            says: rec.says.clone(),
        });
        Ok(rec)
    }

    /// The schedules that still run, soonest first.
    pub fn schedules(&self) -> Vec<ScheduleRecord> {
        let st = self.inner.state.lock().unwrap();
        let mut v: Vec<ScheduleRecord> = st.schedules.values().cloned().collect();
        v.sort_by_key(|s| (s.next_ms, s.created_ms));
        v
    }

    /// Stop a schedule. A run already started goes on (cancel its task to stop it).
    pub fn unschedule(&self, id: &str) -> Result<ScheduleRecord, OrchError> {
        self.inner
            .drop_schedule(id)
            .ok_or_else(|| OrchError::NotAllowed(format!("no schedule `{id}`")))
    }

    /// Start every schedule due at `now_ms`; returns the tasks started. The orchestrator's
    /// timer calls this; an app with its own clock (or a test) can too.
    pub fn tick_schedules(&self, now_ms: u64) -> Vec<String> {
        self.inner.tick_schedules(now_ms)
    }

    /// Run [`tick_schedules`](Self::tick_schedules) every `every` while the orchestrator lives.
    /// [`OrchestratorBuilder::build`] starts one when it runs inside a tokio runtime.
    pub fn spawn_schedule_timer(&self, every: Duration) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { break };
                inner.tick_schedules(now_ms());
                drop(inner);
                tokio::time::sleep(every).await;
            }
        })
    }

    // ---- the board ---------------------------------------------------------------------

    /// The project's board in order: To do, In progress, Done (latest first). See
    /// [`crate::board`].
    pub fn board(&self) -> Vec<WorkItem> {
        let st = self.inner.state.lock().unwrap();
        board::ordered(st.work.values().cloned().collect())
    }

    pub fn work_item(&self, id: &str) -> Option<WorkItem> {
        self.inner.state.lock().unwrap().work.get(id).cloned()
    }

    /// Add an item, at the end of its column unless `change.before` places it. `by` is who adds
    /// it: an agent, [`board::PERSON`], or `chat`.
    pub fn add_work(
        &self,
        by: &str,
        title: &str,
        change: WorkChange,
    ) -> Result<WorkItem, OrchError> {
        self.inner.add_work(by, title, change)
    }

    /// Change an item: its words, column, place, owner or what blocks it. An item an agent works
    /// on keeps that agent as owner until its task ends.
    pub fn change_work(
        &self,
        by: &str,
        id: &str,
        change: WorkChange,
    ) -> Result<WorkItem, OrchError> {
        self.inner.change_work(by, id, change)
    }

    /// Remove an item. Tasks run for it stay as they are.
    pub fn remove_work(&self, id: &str) -> Result<WorkItem, OrchError> {
        self.inner.remove_work(id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stop {
    Cancel,
    Suspend,
    Limit(String),
    /// The process is stopping ([`Engine::stop_soon`]): the task continues after the restart.
    Restart,
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
    /// The agent program its tasks run on, when its model names one (ADR 0043).
    worker: Option<Arc<dyn ark_runtime::worker::Worker>>,
    /// The resolver's default version when `models` was resolved.
    default_version: u64,
    /// Its current call (see [`Step`]).
    pub step: Option<Step>,
}

pub(crate) struct TaskRt {
    pub record: TaskRecord,
    cancel: CancelToken,
    stop: Option<Stop>,
    done: watch::Sender<bool>,
    slots: Arc<tokio::sync::Mutex<Slots>>,
    pub waiting_for: Vec<String>,
    delegations: u32,
    /// Where the person's answer goes while the run that asked is waiting for it.
    answer: Option<tokio::sync::oneshot::Sender<ApprovalDecision>>,
    /// Woken when a question kept across a restart is answered.
    answered: Arc<tokio::sync::Notify>,
    /// Picked up again after a restart: its next run continues the interrupted one.
    recovering: bool,
}

#[derive(Default)]
pub(crate) struct State {
    pub agents: HashMap<String, AgentRt>,
    pub tasks: HashMap<String, TaskRt>,
    pub messages: Vec<crate::graph::MessageEdge>,
    pub schedules: HashMap<String, ScheduleRecord>,
    /// The project's board.
    pub work: HashMap<String, WorkItem>,
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
    /// Tools each agent gets its own of (the web on its model, its own browser), built at each
    /// task from the agent's current model.
    per_agent: Vec<Arc<dyn ark_runtime::per_agent::PerAgent>>,
    /// The agent programs an agent may run on (`agent:<name>`), when the app offers them.
    workers: Option<Arc<dyn ark_runtime::worker::Workers>>,
    /// One agent program at a time in a project folder: what changed there is that task's.
    folders: Mutex<HashMap<std::path::PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    pub max_parallel: usize,
    global: Arc<Semaphore>,
    accounts: Mutex<HashMap<String, Arc<Semaphore>>>,
    pub state: Mutex<State>,
    events: broadcast::Sender<OrchEvent>,
    stream: String,
    /// Held while schedules are ticked: one tick at a time.
    ticks: Mutex<()>,
    /// (task, chat): a turn of that chat waits for the task in this process and takes its
    /// result as its call's answer, so it is not posted too. Lost with the process, as the
    /// turn is: then the result is posted ([`ChatWait`]).
    chat_waits: Mutex<Vec<(String, String)>>,
}

/// A chat's turn waiting for a task it handed off. Dropped without [`ChatWait::took`] (the
/// turn was cancelled), the result goes into the chat as a post.
pub(crate) struct ChatWait {
    inner: Arc<Inner>,
    task: String,
    chat: String,
    took: bool,
}

impl ChatWait {
    /// The turn has the result: it reached the chat.
    pub(crate) fn took(mut self, rec: &TaskRecord) {
        self.took = true;
        if rec.report_to.as_deref() == Some(self.chat.as_str()) && reports(rec).is_some() {
            self.inner.update_task(&rec.id, |t| t.reported = true);
        }
    }
}

impl Drop for ChatWait {
    fn drop(&mut self) {
        {
            let mut w = self.inner.chat_waits.lock().unwrap();
            if let Some(i) = w
                .iter()
                .position(|(t, c)| *t == self.task && *c == self.chat)
            {
                w.remove(i);
            }
        }
        if !self.took {
            let rec = self
                .inner
                .state
                .lock()
                .unwrap()
                .tasks
                .get(&self.task)
                .map(|t| t.record.clone());
            if let Some(r) = rec.filter(|r| r.state.is_final()) {
                self.inner.report_back(&r);
            }
        }
    }
}

/// Calls a stopped run left open did not finish there.
fn close_open_calls(t: &mut TaskRecord) {
    for c in t.tool_calls.iter_mut().filter(|c| c.ok.is_none()) {
        c.ok = Some(false);
        c.summary = "stopped: the process stopped".into();
    }
}

/// What a finished task reports to its chat: (ok, text). Nothing for one that was cancelled
/// (whoever cancelled it knows) or has not finished.
fn reports(t: &TaskRecord) -> Option<(bool, String)> {
    match t.state {
        TaskState::Done => Some((true, t.result.clone().unwrap_or_default())),
        TaskState::Failed => Some((false, t.error.clone().unwrap_or_default())),
        _ => None,
    }
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
    /// Not an end: the process is stopping and the task continues after the restart.
    fn left() -> Self {
        Final {
            state: TaskState::Queued,
            result: None,
            error: None,
        }
    }
}

/// The open task another agent runs for `item`, if any: then `owner` may not take it.
fn claimable(st: &State, item: &str, owner: &str) -> Result<(), OrchError> {
    let w = st
        .work
        .get(item)
        .ok_or_else(|| OrchError::NotAllowed(format!("no board item `{item}`")))?;
    let busy = w.tasks.iter().rev().find_map(|t| {
        st.tasks
            .get(t)
            .filter(|t| !t.record.state.is_final() && t.record.owner != owner)
            .map(|t| (t.record.owner.clone(), t.record.id.clone()))
    });
    match busy {
        Some((other, task)) => Err(OrchError::NotAllowed(format!(
            "{} is being worked on by {other} ({task}); wait for it or pick another item",
            w.id
        ))),
        None => Ok(()),
    }
}

/// The rank that puts `w` before `before` in its column, or at the column's end.
fn column_rank(st: &State, w: &WorkItem, before: Option<&str>) -> f64 {
    let mut column: Vec<&WorkItem> = st
        .work
        .values()
        .filter(|o| o.state == w.state && o.id != w.id)
        .collect();
    column.sort_by(|a, b| {
        a.rank
            .partial_cmp(&b.rank)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rank_before(&column, before)
}

/// Apply the fields of `c` other than state and place, checked.
fn apply_change(st: &State, w: &mut WorkItem, by: &str, c: WorkChange) -> Result<(), OrchError> {
    if let Some(t) = c.title {
        let t = t.trim();
        if t.is_empty() {
            return Err(OrchError::NotAllowed("an item needs a title".into()));
        }
        w.title = clip(t, 200);
    }
    if let Some(n) = c.note {
        w.note = clip(&n, 4000);
    }
    if let Some(o) = c.owner {
        let o = o.trim();
        let owner = match o {
            "" => None,
            PERSON | "you" | "person" => Some(PERSON.to_string()),
            name => {
                let live = st
                    .agents
                    .get(name)
                    .is_some_and(|a| a.record.state != AgentState::Terminated);
                if !live {
                    return Err(OrchError::NotAllowed(format!("no agent named `{name}`")));
                }
                Some(name.to_string())
            }
        };
        // The agent running a task for it keeps it until that task ends.
        if owner != w.owner {
            if let Some(t) = w.tasks.iter().rev().find_map(|t| {
                st.tasks
                    .get(t)
                    .filter(|t| !t.record.state.is_final())
                    .map(|t| (t.record.owner.clone(), t.record.id.clone()))
            }) {
                return Err(OrchError::NotAllowed(format!(
                    "{} is working on it ({}); cancel that task first",
                    t.0, t.1
                )));
            }
        }
        w.set_owner(by, owner);
    }
    if let Some(b) = c.blocked {
        w.set_blocked(by, Some(b));
    }
    Ok(())
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
            // A board item is stored with its last step only: the store holds the item, the
            // stream what changed.
            let slim = match &ev.kind {
                EventKind::Work { item, by } if item.log.len() > 1 => {
                    let mut item = item.clone();
                    item.log.drain(..item.log.len() - 1);
                    Some(OrchEvent {
                        kind: EventKind::Work {
                            item,
                            by: by.clone(),
                        },
                        ..ev.clone()
                    })
                }
                _ => None,
            };
            match serde_json::to_value(slim.as_ref().unwrap_or(&ev)) {
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

    fn save_schedule(&self, r: &ScheduleRecord) {
        match serde_json::to_value(r) {
            Ok(v) => {
                if let Err(e) =
                    self.engine
                        .store
                        .put_record(SCHEDULE, &r.id, self.project.id.as_str(), &v)
                {
                    tracing::warn!(schedule = %r.id, "could not save the schedule: {e}");
                }
            }
            Err(e) => tracing::warn!("could not encode a schedule: {e}"),
        }
    }

    fn save_work(&self, w: &WorkItem, by: &str) {
        match serde_json::to_value(w) {
            Ok(v) => {
                if let Err(e) =
                    self.engine
                        .store
                        .put_record(WORK, &w.id, self.project.id.as_str(), &v)
                {
                    tracing::warn!(item = %w.id, "could not save the board item: {e}");
                }
            }
            Err(e) => tracing::warn!("could not encode a board item: {e}"),
        }
        self.emit(EventKind::Work {
            item: w.clone(),
            by: by.to_string(),
        });
    }

    /// Remove a schedule, from memory and the store under one lock, so a tick running at the
    /// same moment cannot put it back.
    fn drop_schedule(&self, id: &str) -> Option<ScheduleRecord> {
        let rec = {
            let mut st = self.state.lock().unwrap();
            let rec = st.schedules.remove(id)?;
            let _ = self.engine.store.delete_record(SCHEDULE, &rec.id);
            rec
        };
        self.emit(EventKind::Unscheduled {
            schedule: rec.id.clone(),
            agent: rec.agent.clone(),
            says: rec.says.clone(),
        });
        Some(rec)
    }

    /// Start the schedules due at `now`. A schedule whose last run is still open skips this
    /// one; a run missed while the process was down runs once. One tick at a time; a schedule
    /// stopped meanwhile stays stopped.
    fn tick_schedules(&self, now: u64) -> Vec<String> {
        let _one = self.ticks.lock().unwrap();
        let due: Vec<ScheduleRecord> = {
            let st = self.state.lock().unwrap();
            st.schedules
                .values()
                .filter(|s| s.due(now))
                .cloned()
                .collect()
        };
        let mut started = Vec::new();
        for mut s in due {
            if !self.state.lock().unwrap().schedules.contains_key(&s.id) {
                continue;
            }
            let busy = s.last_task.as_ref().is_some_and(|t| {
                let st = self.state.lock().unwrap();
                st.tasks.get(t).is_some_and(|t| !t.record.state.is_final())
            });
            if busy {
                s.skipped += 1;
            } else {
                match self.create_task(&s.agent, s.task.clone(), Origin::External, None) {
                    Ok(id) => {
                        self.update_task(&id, |t| t.schedule = Some(s.id.clone()));
                        self.start(&id);
                        s.runs += 1;
                        s.last_ms = Some(now);
                        s.last_task = Some(id.clone());
                        started.push(id);
                    }
                    Err(e) => {
                        tracing::warn!(schedule = %s.id, "a scheduled run could not start: {e}");
                        s.skipped += 1;
                    }
                }
            }
            s.advance(now);
            let mut st = self.state.lock().unwrap();
            if !st.schedules.contains_key(&s.id) {
                continue;
            }
            if s.active() {
                self.save_schedule(&s);
                st.schedules.insert(s.id.clone(), s);
            } else {
                let _ = self.engine.store.delete_record(SCHEDULE, &s.id);
                st.schedules.remove(&s.id);
            }
        }
        started
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

    /// Load the project's agents and tasks. Work a process that is gone left open is picked up
    /// again (ADR 0036): it is returned, for [`Inner::recover`] to run. A task that stopped
    /// during a state-changing call is failed instead: that change may or may not have
    /// happened, and only the person can tell.
    fn load(&self) -> Result<Vec<String>, OrchError> {
        let store = &self.engine.store;
        let pid = std::process::id();
        let mut interrupted = Vec::new();
        let mut recover = Vec::new();
        let mut late = Vec::new();
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
                        worker: None,
                        default_version: 0,
                        step: None,
                    },
                );
            }
            for v in store.list_records(TASK, Some(self.project.id.as_str()))? {
                let Ok(mut r) = serde_json::from_value::<TaskRecord>(v) else {
                    continue;
                };
                let mut recovering = false;
                if r.state.is_final() {
                    if r.report_to.is_some() && !r.reported && reports(&r).is_some() {
                        late.push(r.id.clone());
                    }
                } else {
                    // Its result has not been anywhere yet.
                    r.reported = false;
                    let alive = r.pid.is_some_and(|p| p != pid && process_alive(p));
                    if !alive {
                        r.pid = None;
                        close_open_calls(&mut r);
                        carry_decisions(&mut r);
                        let asking = r.question.as_ref().is_some_and(Question::open);
                        let scope = st.agents.get(&r.owner).map(|a| a.record.scope.clone());
                        let unsure = match (scope, r.started_ms) {
                            (Some(scope), Some(started)) if !asking => {
                                self.change_cut_short(&ProjectId(scope), started)
                            }
                            _ => None,
                        };
                        if let Some(action) = unsure {
                            r.state = TaskState::Failed;
                            r.error = Some(format!(
                                "{INTERRUPTED} during {action}, which may or may not have \
                                 happened. Check, then run the task again"
                            ));
                            r.waiting_on = None;
                            r.question = None;
                            r.finished_ms = Some(now_ms());
                            interrupted.push(r.clone());
                        } else {
                            // It had started a run: the next one continues it.
                            recovering = r.attempts > 0 && r.state != TaskState::Pending;
                            if asking {
                                r.state = TaskState::Waiting;
                                r.waiting_on = Some(WAITING_PERSON.into());
                            } else if r.state != TaskState::Suspended {
                                r.state = TaskState::Queued;
                                r.waiting_on = Some(RESTARTED.into());
                            }
                            recover.push(r.id.clone());
                        }
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
                        answer: None,
                        answered: Default::default(),
                        recovering,
                    },
                );
            }
            for v in store.list_records(MESSAGE, Some(self.project.id.as_str()))? {
                if let Ok(m) = serde_json::from_value(v) {
                    st.messages.push(m);
                }
            }
            for v in store.list_records(SCHEDULE, Some(self.project.id.as_str()))? {
                if let Ok(s) = serde_json::from_value::<ScheduleRecord>(v) {
                    if s.active() {
                        st.schedules.insert(s.id.clone(), s);
                    }
                }
            }
            for v in store.list_records(WORK, Some(self.project.id.as_str()))? {
                if let Ok(w) = serde_json::from_value::<WorkItem>(v) {
                    st.work.insert(w.id.clone(), w);
                }
            }
        }
        for id in &recover {
            let st = self.state.lock().unwrap();
            if let Some(t) = st.tasks.get(id) {
                self.save_task(&t.record);
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
            self.work_follows(&r);
            self.report_back(&r);
        }
        for id in late {
            self.report_late(&id);
        }
        Ok(recover)
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
        let default_version = self.models.default_version();
        let worker = self.worker_for(spec.model.as_deref())?;
        let models = match &worker {
            Some(w) => Arc::new(elsewhere::models(
                w.as_ref(),
                spec.model.as_deref(),
                &*self.models,
            )),
            None => Arc::new(
                self.models
                    .resolve(&self.lane(&spec.name), spec.model.as_deref())
                    .await?,
            ),
        };
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
                    a.worker = worker.clone();
                    a.default_version = default_version;
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
                            worker: worker.clone(),
                            default_version,
                            step: None,
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
        let (have, spec, version) = {
            let st = self.state.lock().unwrap();
            let a = st
                .agents
                .get(agent)
                .ok_or_else(|| OrchError::UnknownAgent(agent.into()))?;
            (a.models.clone(), a.record.spec.clone(), a.default_version)
        };
        match have {
            // An agent without a model of its own follows the default: when it changed (the
            // person chose another model), the agent moves before its next task, and a vendor
            // session on the old model is released.
            Some(m) if spec.model.is_none() && version != self.models.default_version() => {
                if m.session.is_some() {
                    self.models.close(&self.lane(agent)).await;
                }
            }
            Some(m) => return Ok(m),
            None => {}
        }
        // Loaded from the store, or moving to the new default: resolve, and wire the session
        // events, as spawn does.
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
        if let Some(item) = &spec.item {
            let st = self.state.lock().unwrap();
            claimable(&st, item, owner)?;
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
            schedule: None,
            item: spec.item.clone(),
            report_to: spec.report_to.clone(),
            reported: false,
            question: None,
            decisions: Vec::new(),
        };
        let (parent_rec, linked) = {
            let mut st = self.state.lock().unwrap();
            // Checked again under the lock that inserts the task: two agents never take one item.
            if let Some(item) = &spec.item {
                claimable(&st, item, owner)?;
            }
            let linked = spec.item.as_ref().and_then(|item| {
                let w = st.work.get_mut(item)?;
                w.tasks.push(id.clone());
                w.owner = Some(owner.to_string());
                if w.state != WorkState::Doing {
                    w.state = WorkState::Doing;
                    w.done_ms = None;
                }
                w.blocked = None;
                w.step(
                    owner,
                    format!("started {id}: {}", clip(&spec.objective, 100)),
                );
                Some(w.clone())
            });
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
                    answer: None,
                    answered: Default::default(),
                    recovering: false,
                },
            );
            let parent_rec = parent.and_then(|p| {
                st.tasks.get_mut(p).map(|t| {
                    t.record.children.push(id.clone());
                    t.record.clone()
                })
            });
            (parent_rec, linked)
        };
        if let Some(p) = parent_rec {
            self.save_task(&p);
        }
        self.save_task(&record);
        if let Some(w) = linked {
            self.save_work(&w, owner);
        }
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
            // What an agent program changed is recorded once, when the task ends, however it
            // ends (a cancel or a terminated agent never reach the run).
            if f.state.is_final() {
                me.end_work(&id).await;
            }
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
            // A run the person starts again is new work: what earlier runs did (a change cut
            // short, a change made) is not this run's after a later restart.
            t.record.started_ms = None;
            t.record.reported = false;
            t.record.max_attempts = t.record.attempts + 1;
            t.stop = None;
            t.cancel = CancelToken::new();
            t.done.send_replace(false);
        }
        let rec = self.update_task(id, |_| {});
        if let Some(r) = rec {
            if let Some(item) = &r.item {
                let w = {
                    let mut st = self.state.lock().unwrap();
                    st.work.get_mut(item).map(|w| {
                        w.blocked = None;
                        if w.state != WorkState::Doing {
                            w.state = WorkState::Doing;
                            w.done_ms = None;
                        }
                        w.step(&r.owner, format!("running {} again", r.id));
                        w.clone()
                    })
                };
                if let Some(w) = w {
                    self.save_work(&w, &r.owner);
                }
            }
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
        let mut resumed = self.take_recovering(id);
        if !self.wait_for_answer(id).await {
            return self.stopped_before_start(id);
        }
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
                    // Wake on resume, or on a cancel. A task the suspend stopped mid-run holds
                    // a token that is already cancelled: it waits for the resume (and checks
                    // now and then for a cancel).
                    let cancel = self.cancel_token(id);
                    let fresh = !cancel.is_cancelled();
                    tokio::select! {
                        _ = status.wait_for(|s| *s != AgentState::Suspended) => {}
                        _ = cancel.cancelled(), if fresh => {}
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
            if self.engine.is_stopping() {
                self.release(id).await;
                return Final::left();
            }
            let attempt = self.attempt(id, &owner, &models, resumed).await;
            // Answers carry only into the run that continues one the process stopped in.
            if !matches!(attempt, Attempt::Stopped(Stop::Restart)) {
                self.update_task(id, |t| t.decisions.iter_mut().for_each(|d| d.carry = false));
            }
            self.release(id).await;
            drop(guard);
            match attempt {
                Attempt::Done(answer) => return Final::done(answer),
                Attempt::Stopped(Stop::Suspend) => {
                    // A question it was asking is asked again when it runs again.
                    self.update_task(id, |t| {
                        t.state = TaskState::Suspended;
                        t.question = None;
                    });
                    continue;
                }
                Attempt::Stopped(Stop::Cancel) => return Final::cancelled("cancelled"),
                Attempt::Stopped(Stop::Restart) => return Final::left(),
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

    /// A state-changing call of an agent, since `started_ms`, that a stopped process left
    /// running (the engine marks those when it opens). Whether it happened is unknown.
    fn change_cut_short(&self, scope: &ProjectId, started_ms: u64) -> Option<String> {
        self.engine
            .store
            .list_invocations(scope)
            .ok()?
            .into_iter()
            .filter(|i| i.created_at.unix_timestamp_nanos() / 1_000_000 >= started_ms as i128)
            .find(|i| {
                let left_running = i.status == ark_core::InvocationStatus::Running
                    && !i.pid.is_some_and(process_alive);
                i.risk == ark_core::RiskTier::Mutate
                    && (left_running
                        || i.error.as_deref() == Some(ark_runtime::engine::INTERRUPTED_CALL))
            })
            .map(|i| i.action)
    }

    /// Run again the work [`Inner::load`] picked up after a restart.
    fn recover(&self, ids: &[String]) {
        for id in ids {
            let (owner_known, asking) = {
                let st = self.state.lock().unwrap();
                match st.tasks.get(id) {
                    Some(t) => (
                        st.agents.contains_key(&t.record.owner),
                        t.record.question.as_ref().is_some_and(Question::open),
                    ),
                    None => continue,
                }
            };
            if !owner_known {
                self.finish(
                    id,
                    Final::failed(format!("{INTERRUPTED}; its agent is gone")),
                );
                continue;
            }
            if !asking {
                self.emit(EventKind::TaskQueued {
                    task: id.clone(),
                    reason: RESTARTED.into(),
                });
            }
            self.start(id);
        }
    }

    fn take_recovering(&self, id: &str) -> bool {
        let mut st = self.state.lock().unwrap();
        st.tasks
            .get_mut(id)
            .is_some_and(|t| std::mem::take(&mut t.recovering))
    }

    /// A question kept across a restart: the run that asked is gone, so the task waits here
    /// for the answer. False: cancelled while waiting.
    async fn wait_for_answer(&self, id: &str) -> bool {
        loop {
            let (open, answered, cancel) = {
                let st = self.state.lock().unwrap();
                let t = &st.tasks[id];
                let open = t.record.question.as_ref().is_some_and(Question::open);
                (open, t.answered.clone(), t.cancel.clone())
            };
            if !open {
                return true;
            }
            tokio::select! {
                _ = answered.notified() => {}
                _ = cancel.cancelled() => return false,
            }
        }
    }

    /// The task's agent asks the person. The task keeps the question, so any client can answer
    /// it and a restart does not lose it, and gives its capacity back while it waits.
    async fn ask(
        &self,
        task: &str,
        agent: &str,
        request: &ApprovalRequest,
    ) -> (String, tokio::sync::oneshot::Receiver<ApprovalDecision>) {
        let id = format!("q-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let (tx, rx) = tokio::sync::oneshot::channel();
        if let Some(t) = self.state.lock().unwrap().tasks.get_mut(task) {
            t.answer = Some(tx);
        }
        self.update_task(task, |t| {
            t.question = Some(Question {
                id: id.clone(),
                request: request.clone(),
                asked_ms: now_ms(),
                answer: None,
            });
            t.usage.asked += 1;
        });
        self.park(task, &[ANSWER.to_string()]).await;
        self.emit(EventKind::Asked {
            task: task.to_string(),
            agent: agent.to_string(),
            question: id.clone(),
            preview: request.preview.clone(),
        });
        (id, rx)
    }

    /// The person answered the question the task's run waits on.
    async fn answered(&self, task: &str, question: &str, decision: ApprovalDecision) {
        let ours = {
            let mut st = self.state.lock().unwrap();
            st.tasks.get_mut(task).is_some_and(|t| {
                t.answer = None;
                t.record.question.as_ref().is_some_and(|q| q.id == question)
            })
        };
        if ours {
            self.update_task(task, |t| t.question = None);
        }
        self.emit(EventKind::Answered {
            task: task.to_string(),
            question: question.to_string(),
            decision,
        });
        self.unpark(task, &[ANSWER.to_string()]).await;
    }

    /// An answer given while the run that asked was gone, for the same call asked again.
    fn take_answer(&self, task: &str, request: &ApprovalRequest) -> Option<ApprovalDecision> {
        let decision = {
            let st = self.state.lock().unwrap();
            let q = st.tasks.get(task)?.record.question.as_ref()?;
            let d = q.answer?;
            (q.request.action == request.action && q.request.input == request.input).then_some(d)?
        };
        self.update_task(task, |t| t.question = None);
        Some(decision)
    }

    /// The calls an earlier run of this task made, for the next run not to repeat a change
    /// (`Harness::done_before`). The agent runs one task at a time, so its calls since the
    /// task first started are this task's.
    fn done_before(&self, id: &str, scope: &ProjectId) -> Vec<ark_core::Invocation> {
        let started = {
            let st = self.state.lock().unwrap();
            st.tasks.get(id).and_then(|t| t.record.started_ms)
        };
        let Some(started) = started else {
            return Vec::new();
        };
        self.engine
            .store
            .list_invocations(scope)
            .unwrap_or_default()
            .into_iter()
            .filter(|i| i.created_at.unix_timestamp_nanos() / 1_000_000 >= started as i128)
            .collect()
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
        let worker;
        let (task, agent, deps, children, inbox, cancel, roster, item) = {
            let mut st = self.state.lock().unwrap();
            let task = st.tasks[id].record.clone();
            let item = task.item.as_ref().and_then(|w| st.work.get(w).cloned());
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
            worker = a.worker.clone();
            (
                task,
                a.record.clone(),
                deps,
                children,
                inbox,
                cancel,
                roster,
                item,
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
        // The app's tools and the plugins connected now, as this agent may use them.
        let mut registry: Registry = self
            .engine
            .tools()
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
        // Its own web search and browser, as far as its `tools` allow.
        let owner_name = format!("{}/{}", self.project.id, owner);
        let mut per_agent_actions: Vec<String> = Vec::new();
        let mut per_agent_distillers = Vec::new();
        // What it can ask to turn on mid-task (the Computer), as far as its `tools` allow.
        let mut turn_on: Vec<Arc<dyn ark_runtime::AcquisitionProvider>> = Vec::new();
        for p in &self.per_agent {
            let model = models.router.provider(ark_runtime::Role::Plan);
            let who = ark_runtime::per_agent::Owner {
                name: &owner_name,
                model,
            };
            if let Some(acq) = p.acquisition(&who) {
                let may = p
                    .listing()
                    .actions()
                    .iter()
                    .any(|d| spec.tools.iter().any(|pat| action_matches(pat, &d.name)));
                if may {
                    turn_on.push(Arc::new(Allowed {
                        inner: acq,
                        tools: spec.tools.clone(),
                    }));
                }
            }
            let adapter = p.for_owner(&who);
            let allowed: Vec<String> = adapter
                .actions()
                .into_iter()
                .map(|d| d.name)
                .filter(|a| spec.tools.iter().any(|pat| action_matches(pat, a)))
                .collect();
            if allowed.is_empty() {
                continue;
            }
            for a in Registry::new()
                .with(adapter)
                .filtered(|a| allowed.iter().any(|x| x == a))
                .adapters()
            {
                registry.register(a.clone());
            }
            per_agent_actions.extend(allowed);
            per_agent_distillers.extend(p.distiller());
        }
        let sets = self.rules_for(&task);
        let objective = format!("{}\n{}", task.objective, task.context);
        let selection = (!sets.is_empty()).then(|| select(&sets, &objective, RULES_BUDGET));
        if !sets.is_empty() {
            registry.register(Arc::new(RulesTool::new(sets)));
        }
        // Skills it may read: their catalog in the first message, their instructions on demand.
        let skills = if spec.skills.is_empty() {
            Vec::new()
        } else {
            skills::allowed(
                &skills::discover(&skills::dirs(&self.engine, &self.project.id)),
                &spec.skills,
            )
        };
        let catalog = skills::catalog(&skills, skills::CATALOG_BUDGET).map(|c| c.text);
        if !skills.is_empty() {
            registry.register(Arc::new(skills::SkillsTool::new(skills)));
        }
        let engine = self.engine.scoped(registry);
        let mut profile = self.agent_profile(spec, &delegates);
        if !turn_on.is_empty() {
            // The app's sources first (accounts, installs), then the agent's own.
            let mut sources: Vec<_> = profile.acquisition.take().into_iter().collect();
            sources.extend(turn_on);
            profile.acquisition = Some(Arc::new(ark_runtime::Sources(sources)));
        }
        if catalog.is_some() {
            // The catalog says to call it, so it is loaded, not left to discovery.
            profile.distillers.push(skills::distiller());
            profile.core_actions.push(skills::READ.into());
        }
        for d in per_agent_distillers {
            profile.distillers.push(d);
        }
        profile.core_actions.extend(per_agent_actions);
        let scope = ProjectId(agent.scope.clone());

        // A run that continues an interrupted one gets the changes it already made, so it does
        // not make them twice.
        let done = if resumed {
            self.done_before(id, &scope)
        } else {
            Vec::new()
        };
        let done_lines: Vec<String> = done
            .iter()
            .filter(|i| {
                i.risk == ark_core::RiskTier::Mutate && i.status == ark_core::InvocationStatus::Ok
            })
            .map(|i| format!("{} ({})", i.action, clip(&i.output.to_string(), 160)))
            .collect();
        let dep_refs: Vec<&TaskRecord> = deps.iter().collect();
        let brief = Brief {
            done: &done_lines,
            task: &task,
            deps: dep_refs,
            children: children.iter().collect(),
            inbox: &inbox,
            resumed,
            item: item.as_ref(),
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
                        orch: self.clone(),
                        agent: owner.to_string(),
                        task: id.to_string(),
                        gate: gate.clone(),
                    }))))
                }
                _ => None,
            })
            .cancel(Some(cancel.clone()))
            .stopping(Some(self.engine.stopping()))
            .steering(Some(steer.clone()))
            .conversation(session.conversation_digest())
            .rules(selection.clone())
            .skills(catalog)
            .done_before(done);

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
        // An agent program does the task with its own tools; ARK keeps the task (ADR 0043).
        if let Some(w) = worker {
            let ran = self
                .run_worker(
                    id,
                    owner,
                    w,
                    &scope,
                    &brief,
                    &cancel,
                    spec.allow_mutate,
                    &models.model,
                )
                .await;
            if let Some(t) = timer {
                t.abort();
            }
            if let Some(a) = self.state.lock().unwrap().agents.get_mut(owner) {
                a.steering = None;
                // A program takes no messages while it works: they wait for the next task.
                for text in steer.take_undelivered() {
                    a.record.inbox.push(Mail {
                        from: "?".into(),
                        text,
                        task: None,
                        at_ms: now_ms(),
                    });
                }
            }
            return ran;
        }
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
                Attempt::Stopped(
                    self.stop_reason(id)
                        .unwrap_or(if self.engine.is_stopping() {
                            Stop::Restart
                        } else {
                            Stop::Cancel
                        }),
                )
            }
            Err(e) => {
                let _ = session.fail_turn(&self.engine, &brief, &e.to_string());
                match self.stop_reason(id) {
                    Some(s) => Attempt::Stopped(s),
                    None if self.engine.is_stopping() => Attempt::Stopped(Stop::Restart),
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
                    HarnessEvent::ToolCall { action, input, .. } => {
                        a.step = Some(Step {
                            task: task.to_string(),
                            action: action.clone(),
                            input: clip_strings(input),
                            since_ms: now_ms(),
                        });
                    }
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
                    // A file's version before the call is kept for Restore (ADR 0034); the
                    // task did not make it.
                    if art.meta.get("before").and_then(|b| b.as_bool()) == Some(true) {
                        continue;
                    }
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
        if !f.state.is_final() {
            return self.leave(id);
        }
        let rec = self.update_task(id, |t| {
            t.state = f.state;
            t.result = f.result.clone();
            t.error = f.error.clone();
            t.waiting_on = None;
            t.finished_ms = Some(now_ms());
            t.pid = None;
            t.question = None;
        });
        if let Some(t) = self.state.lock().unwrap().tasks.get_mut(id) {
            t.answer = None;
        }
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
        self.work_follows(&rec);
        self.report_back(&rec);
        let st = self.state.lock().unwrap();
        if let Some(t) = st.tasks.get(id) {
            t.done.send_replace(true);
        }
    }

    /// A task that reports to a chat gets its result there once, when it ends: done or failed,
    /// not cancelled (whoever cancelled it knows). A turn of that chat waiting for it in this
    /// process takes it as its call's answer ([`ChatWait`]); otherwise it is posted.
    fn report_back(&self, t: &TaskRecord) {
        let Some(chat) = &t.report_to else { return };
        let Some((ok, text)) = reports(t).filter(|_| !t.reported) else {
            return;
        };
        let waiting = self
            .chat_waits
            .lock()
            .unwrap()
            .iter()
            .any(|(task, c)| *task == t.id && c == chat);
        if waiting {
            return;
        }
        let post = Post::new(&t.owner, clip(&t.objective, 80), text, ok).work(&t.id);
        match ark_runtime::session::post(&self.engine, chat, post.clone()) {
            Ok(_) => {
                self.update_task(&t.id, |r| r.reported = true);
                self.emit(EventKind::Posted {
                    session: chat.clone(),
                    task: t.id.clone(),
                    post,
                })
            }
            Err(e) => tracing::warn!(task = %t.id, "could not post into the chat: {e}"),
        }
    }

    /// A result that had not reached its chat when the process stopped: posted now, unless the
    /// post was made and only its mark was lost.
    fn report_late(&self, id: &str) {
        let Some(rec) = self
            .state
            .lock()
            .unwrap()
            .tasks
            .get(id)
            .map(|t| t.record.clone())
        else {
            return;
        };
        let Some(chat) = &rec.report_to else { return };
        match ark_runtime::session::posted(&self.engine, chat, id) {
            Ok(true) => {
                self.update_task(id, |r| r.reported = true);
            }
            Ok(false) => self.report_back(&rec),
            Err(e) => tracing::warn!(task = %id, "could not read the chat's posts: {e}"),
        }
    }

    /// Wait in a turn of `chat` for a task it handed off: the result is that turn's, not posted.
    pub(crate) fn chat_wait(self: &Arc<Self>, task: &str, chat: &str) -> ChatWait {
        self.chat_waits
            .lock()
            .unwrap()
            .push((task.to_string(), chat.to_string()));
        ChatWait {
            inner: self.clone(),
            task: task.to_string(),
            chat: chat.to_string(),
            took: false,
        }
    }

    /// The process is stopping and the task's run stopped at a safe point: it waits for the next
    /// process, as a restart leaves it ([`Inner::load`]), and is not failed.
    fn leave(&self, id: &str) {
        let rec = self.update_task(id, |t| {
            t.pid = None;
            close_open_calls(t);
            carry_decisions(t);
            if t.question.as_ref().is_some_and(Question::open) {
                t.state = TaskState::Waiting;
                t.waiting_on = Some(WAITING_PERSON.into());
            } else if t.state != TaskState::Suspended {
                t.state = TaskState::Queued;
                t.waiting_on = Some(RESTARTED.into());
            }
        });
        let Some(rec) = rec else { return };
        let owner = rec.owner.clone();
        let current = {
            let st = self.state.lock().unwrap();
            st.agents
                .get(&owner)
                .and_then(|a| a.record.current_task.clone())
        };
        if current.as_deref() == Some(id) {
            self.update_agent(&owner, |a| a.current_task = None);
            if self.agent_state(&owner) == Some(AgentState::Running) {
                self.set_agent_state(&owner, AgentState::Idle);
            }
        }
        if rec.state == TaskState::Queued {
            self.emit(EventKind::TaskQueued {
                task: id.to_string(),
                reason: RESTARTED.into(),
            });
        }
    }

    // ---- the board ------------------------------------------------------------------

    /// A finished task moves its board item (see [`crate::board`]): the last open task done
    /// makes it done; a failure blocks it; a cancel puts it back in To do.
    fn work_follows(&self, t: &TaskRecord) {
        let Some(item) = &t.item else { return };
        let changed = {
            let mut st = self.state.lock().unwrap();
            let others_open = st.work.get(item).is_some_and(|w| {
                w.tasks.iter().any(|x| {
                    x != &t.id && st.tasks.get(x).is_some_and(|o| !o.record.state.is_final())
                })
            });
            let Some(w) = st.work.get_mut(item) else {
                return;
            };
            match t.state {
                TaskState::Done => {
                    let said = clip(t.result.as_deref().unwrap_or(""), 200);
                    w.step(&t.owner, format!("finished {}: {said}", t.id));
                    if !others_open && w.state != WorkState::Done {
                        w.state = WorkState::Done;
                        w.done_ms = Some(now_ms());
                        w.blocked = None;
                    }
                }
                TaskState::Failed => {
                    let why = format!(
                        "{} failed: {}",
                        t.id,
                        clip(t.error.as_deref().unwrap_or("no reason given"), 200)
                    );
                    w.step(&t.owner, why.clone());
                    if w.state != WorkState::Done {
                        w.blocked = Some(why);
                    }
                }
                TaskState::Cancelled => {
                    w.step(&t.owner, format!("{} was cancelled", t.id));
                    if !others_open && w.state == WorkState::Doing {
                        w.state = WorkState::Todo;
                    }
                }
                _ => return,
            }
            w.clone()
        };
        self.save_work(&changed, &t.owner);
    }

    pub(crate) fn add_work(
        &self,
        by: &str,
        title: &str,
        change: WorkChange,
    ) -> Result<WorkItem, OrchError> {
        if title.trim().is_empty() {
            return Err(OrchError::NotAllowed("an item needs a title".into()));
        }
        let w = {
            let mut st = self.state.lock().unwrap();
            let mut w = WorkItem::new(self.project.id.as_str(), by, title.trim());
            let mut change = change;
            change.title = None;
            // A new item goes at the end of its column unless placed.
            let state = change.state.take().unwrap_or(WorkState::Todo);
            w.state = state;
            if state == WorkState::Done {
                w.done_ms = Some(now_ms());
            }
            w.rank = column_rank(&st, &w, change.before.as_deref());
            change.before = None;
            apply_change(&st, &mut w, by, change)?;
            st.work.insert(w.id.clone(), w.clone());
            w
        };
        self.save_work(&w, by);
        Ok(w)
    }

    pub(crate) fn change_work(
        &self,
        by: &str,
        id: &str,
        change: WorkChange,
    ) -> Result<WorkItem, OrchError> {
        let w = {
            let mut st = self.state.lock().unwrap();
            let mut w = st
                .work
                .get(id)
                .cloned()
                .ok_or_else(|| OrchError::NotAllowed(format!("no board item `{id}`")))?;
            let moved = change.state.is_some() || change.before.is_some();
            let mut change = change;
            if let Some(s) = change.state.take() {
                w.set_state(by, s);
            }
            if moved {
                w.rank = column_rank(&st, &w, change.before.as_deref());
            }
            change.before = None;
            apply_change(&st, &mut w, by, change)?;
            w.updated_ms = now_ms();
            st.work.insert(w.id.clone(), w.clone());
            w
        };
        self.save_work(&w, by);
        Ok(w)
    }

    pub(crate) fn remove_work(&self, id: &str) -> Result<WorkItem, OrchError> {
        let w = {
            let mut st = self.state.lock().unwrap();
            let w = st
                .work
                .remove(id)
                .ok_or_else(|| OrchError::NotAllowed(format!("no board item `{id}`")))?;
            let _ = self.engine.store.delete_record(WORK, id);
            w
        };
        self.emit(EventKind::WorkRemoved {
            item: w.id.clone(),
            title: w.title.clone(),
        });
        Ok(w)
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
            t.waiting_on = Some(waiting_text(&on));
        });
        self.set_agent_state(&owner, AgentState::Waiting);
        if ids.iter().any(|i| i != ANSWER) {
            self.emit(EventKind::TaskWaiting {
                task: parent.to_string(),
                on: ids.to_vec(),
            });
        }
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
            self.update_task(parent, |t| t.waiting_on = Some(waiting_text(&on)));
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
/// While the person has not answered, the task keeps the question and waits (ADR 0036).
struct OnBehalf {
    orch: Arc<Inner>,
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
        // Answered while no run waited (after a restart): the same call needs no new question.
        if let Some(d) = self.orch.take_answer(&self.task, &r) {
            self.orch.decided(&self.task, &r, d);
            return d;
        }
        // Answered in an earlier run of this task: the person is not asked twice.
        if let Some(d) = self.orch.decided_before(&self.task, &r) {
            return d;
        }
        // A grant answers without the person: nothing to wait for.
        if !r.ask_each_time && self.gate.is_granted(&r.class) {
            return self.gate.check(&r).await;
        }
        let (question, answer) = self.orch.ask(&self.task, &self.agent, &r).await;
        let decision = tokio::select! {
            d = self.gate.check(&r) => d,
            Ok(d) = answer => {
                // Answered through the orchestrator (another client, the terminal).
                self.gate.remember(&r, d);
                d
            }
        };
        self.orch.answered(&self.task, &question, decision).await;
        self.orch.decided(&self.task, &r, decision);
        decision
    }
}

/// The process stops in this run: its answers carry into the run that continues it.
fn carry_decisions(t: &mut TaskRecord) {
    let run = t.attempts;
    for d in &mut t.decisions {
        d.carry |= d.attempt == run;
    }
}

/// Why a task left running by a stopped process failed.
pub const INTERRUPTED: &str = "interrupted: the process stopped while it ran";

/// What a task waits on while its question waits for the person.
pub const WAITING_PERSON: &str = "your answer";

/// Why a task is queued again after a restart.
pub const RESTARTED: &str = "continues after a restart";

/// The entry a task's `waiting_for` holds while it waits for the person's answer.
const ANSWER: &str = "@answer";

/// A call's input with long strings clipped, for showing what an agent does.
fn clip_strings(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) => serde_json::Value::String(clip(s, 200)),
        serde_json::Value::Array(a) => a.iter().take(20).map(clip_strings).collect(),
        serde_json::Value::Object(m) => m
            .iter()
            .map(|(k, x)| (k.clone(), clip_strings(x)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
        other => other.clone(),
    }
}

/// What a task waits on, in words: the person's answer, tasks it delegated, or both.
fn waiting_text(on: &[String]) -> String {
    let tasks: Vec<&str> = on
        .iter()
        .filter(|w| *w != ANSWER)
        .map(String::as_str)
        .collect();
    let mut parts = Vec::new();
    if on.iter().any(|w| w == ANSWER) {
        parts.push(WAITING_PERSON.to_string());
    }
    if !tasks.is_empty() {
        parts.push(format!("tasks {}", tasks.join(", ")));
    }
    parts.join("; ")
}

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

/// An agent's own acquisition source, handing back only the actions its `tools` allow.
struct Allowed {
    inner: Arc<dyn ark_runtime::AcquisitionProvider>,
    tools: Vec<String>,
}

#[async_trait::async_trait]
impl ark_runtime::AcquisitionProvider for Allowed {
    async fn find(&self, need: &str) -> String {
        self.inner.find(need).await
    }
    async fn propose(&self, need: &str) -> String {
        self.inner.propose(need).await
    }
    async fn acquire(&self, need: &str, approved: bool) -> ark_runtime::AcquisitionResult {
        let mut got = self.inner.acquire(need, approved).await;
        let mut all = Registry::new();
        for t in got.tools.drain(..) {
            all.register(t);
        }
        got.tools = all
            .filtered(|a| self.tools.iter().any(|pat| action_matches(pat, a)))
            .adapters()
            .to_vec();
        got
    }
    fn active(&self) -> bool {
        self.inner.active()
    }
    fn offer(&self) -> Option<String> {
        self.inner.offer()
    }
    fn knows(&self, need: &str) -> bool {
        self.inner.knows(need)
    }
    fn asks_a_person(&self, need: &str) -> bool {
        self.inner.asks_a_person(need)
    }
}

#[cfg(test)]
mod allowed_tests {
    use super::*;
    use ark_core::{ActionDescriptor, RiskTier};
    use ark_runtime::{AcquisitionProvider, AcquisitionResult};

    struct Two;
    #[async_trait::async_trait]
    impl ark_tools::Adapter for Two {
        fn name(&self) -> &str {
            "computer"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            ["computer.look", "computer.click"]
                .map(|n| ActionDescriptor::new(n, n, RiskTier::Mutate))
                .to_vec()
        }
        async fn run(
            &self,
            a: &str,
            _: serde_json::Value,
            _: &ark_tools::RunCtx<'_>,
        ) -> ark_tools::Result<ark_tools::ActionOutput> {
            Err(ark_tools::AdapterError::UnknownAction(a.into()))
        }
    }

    struct On;
    #[async_trait::async_trait]
    impl AcquisitionProvider for On {
        async fn find(&self, _: &str) -> String {
            String::new()
        }
        async fn propose(&self, _: &str) -> String {
            String::new()
        }
        async fn acquire(&self, _: &str, _: bool) -> AcquisitionResult {
            AcquisitionResult::changed("on").with_tools(vec![Arc::new(Two)])
        }
        fn asks_a_person(&self, _: &str) -> bool {
            true
        }
    }

    /// An agent allowed only `computer.look` gets only that once the Computer is on.
    #[tokio::test]
    async fn an_agent_gets_only_the_acquired_actions_its_tools_allow() {
        let a = Allowed {
            inner: Arc::new(On),
            tools: vec!["computer.look".into()],
        };
        let got = a.acquire("computer", true).await;
        let names: Vec<String> = got
            .tools
            .iter()
            .flat_map(|t| t.actions())
            .map(|d| d.name)
            .collect();
        assert_eq!(names, ["computer.look"]);
        assert!(a.asks_a_person("computer"));
    }
}
