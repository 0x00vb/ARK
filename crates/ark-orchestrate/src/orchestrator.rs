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
use ark_runtime::process;
use ark_runtime::rules::{select, RULES_BUDGET};
use ark_runtime::skills;
use ark_runtime::{
    AgentProfile, ApprovalDecision, ApprovalGate, ApprovalRequest, Approver, CancelToken, Engine,
    Harness, HarnessEvent, Post, RuleBook, RulesTool, Session, Steering,
};
use ark_sdk::Ark;
use ark_tools::{action_matches, Registry};
use tokio::sync::{broadcast, watch, OwnedSemaphorePermit, Semaphore};

use crate::board::{
    self, rank_before, Board, WorkChange, WorkItem, WorkState, BOARD, FIRST_GOAL, PERSON, WORK,
};
use crate::brief::{clip, Brief};
use crate::event::{EventKind, OrchEvent};
use crate::models::{AgentModels, ModelResolver, Models, ModelsBuilder};
use crate::record::{
    now_ms, AgentRecord, AgentState, ArtifactRef, Mail, Origin, Question, Step, TaskRecord,
    TaskState, ToolCallRecord, Usage, TOOL_CALLS_KEPT,
};
use crate::schedule::{IfMissed, ScheduleChange, ScheduleRecord, When, SCHEDULE};
use crate::spec::{AgentSpec, TaskSpec};
use crate::tools::{self, AgentTools};
use crate::OrchError;

mod admission;
mod authority;
mod automation_attention;
mod automation_children;
pub use automation_attention::{
    automation_attention_runs, automation_projects_with_work, automation_question,
    stored_automation_run,
};
mod automation_reports;
mod automation_schedule;
mod automations;
mod elsewhere;
mod general;
mod legacy_automation;
mod runs;
mod webhooks;
pub use admission::{WorkReceipt, WorkTarget};
pub use general::{GeneralWorkResult, GeneralWorkStart, GeneralWorkState};
mod ownership;
mod resolution;
mod results;
pub use resolution::{WorkFailure, WorkRunRef};
mod work;
mod work_snapshot;
pub use work_snapshot::{WorkEntry, WorkPage, WorkTemplate};

pub(crate) const AGENT: &str = "orch_agent";
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
        let ticks = ownership::schedule_lock(&engine, project.id.as_str())?;
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
            holding: Mutex::new(HashMap::new()),
            max_parallel: self.max_parallel,
            global: Arc::new(Semaphore::new(self.max_parallel)),
            accounts: Mutex::new(HashMap::new()),
            chat_waits: Mutex::new(Vec::new()),
            state: Mutex::new(State::default()),
            events,
            ticks,
            conversations: tokio::sync::Mutex::new(()),
            execution_token: uuid::Uuid::new_v4().to_string(),
        });
        let recover = inner.load()?;
        let orch = Orchestrator { inner };
        // Schedules fire whether or not anything else runs: no window needs to be open. Work a
        // stopped process left open continues the same way.
        if tokio::runtime::Handle::try_current().is_ok() {
            orch.spawn_schedule_timer(SCHEDULE_TICK);
            orch.inner.recover(&recover);
            orch.recover_automations()?;
            orch.inner.deliver_automation_reports()?;
        }
        Ok(orch)
    }
}

/// How often the timer looks for schedules that are due.
const SCHEDULE_TICK: Duration = Duration::from_secs(20);

/// Runs of one schedule open at once when it allows overlap: the one running and one waiting.
const MAX_OPEN_RUNS: usize = 2;

/// Finished runs kept per schedule.
const RUNS_KEPT: usize = 50;

/// Runs in a row that did not finish before a schedule pauses itself.
const MAX_FAILURES: u32 = 3;

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

    /// Change only an idle agent's model without racing task admission or other edits.
    pub async fn select_idle_model(
        &self,
        name: &str,
        model: &str,
        request: &str,
    ) -> Result<AgentRecord, OrchError> {
        self.inner.require_agent_control(name)?;
        let _guard = self.inner.conversations.lock().await;
        let record = self
            .agent(name)
            .ok_or_else(|| OrchError::UnknownAgent(name.into()))?;
        if let Some(effect) = self
            .inner
            .engine
            .store
            .get_record("orch_model_change", request)?
        {
            if effect["agent"] != name
                || effect["project"] != self.project().id.as_str()
                || effect["model"] != model
            {
                return Err(OrchError::NotAllowed(
                    "model change request conflict".into(),
                ));
            }
            return Ok(record);
        }
        if record.state != AgentState::Idle {
            return Err(OrchError::NotAllowed("the agent is busy".into()));
        }
        let mut spec = record.spec;
        spec.model = Some(model.to_string());
        self.inner
            .spawn_record(spec, None, None, Some(request), None)
            .await
    }

    /// Create once without overwriting an unrelated agent or later edits.
    pub async fn create_once(
        &self,
        spec: AgentSpec,
        request: &str,
    ) -> Result<AgentRecord, OrchError> {
        let _guard = self.inner.conversations.lock().await;
        if let Some(record) = self.agent(&spec.name) {
            return if record.creation_request.as_deref() == Some(request) {
                Ok(record)
            } else {
                Err(OrchError::NotAllowed(
                    "an agent with that name already exists".into(),
                ))
            };
        }
        self.inner
            .spawn_record(spec, None, Some(request), None, None)
            .await
    }

    /// Create a named chat agent once, validating an explicit catalog selection.
    pub async fn create_chat_agent(
        &self,
        spec: AgentSpec,
        request: &str,
    ) -> Result<AgentRecord, OrchError> {
        let _guard = self.inner.conversations.lock().await;
        let digest = blake3::hash(&serde_json::to_vec(&spec)?)
            .to_hex()
            .to_string();
        if let Some(effect) = self
            .inner
            .engine
            .store
            .get_record("orch_chat_creation", request)?
        {
            if effect["digest"] != digest || effect["project"] != self.project().id.as_str() {
                return Err(OrchError::NotAllowed(
                    "that creation request has different inputs".into(),
                ));
            }
            let name = effect["agent"]
                .as_str()
                .ok_or_else(|| OrchError::NotAllowed("invalid creation receipt".into()))?;
            return self
                .agent(name)
                .ok_or_else(|| OrchError::UnknownAgent(name.into()));
        }
        if self.agent(&spec.name).is_some() {
            return Err(OrchError::NotAllowed(
                "an agent with that name already exists".into(),
            ));
        }
        if let Some(model) = &spec.model {
            self.inner.models.validate_selection(model).await?;
        }
        let effect =
            serde_json::json!({"agent":spec.name,"project":self.project().id,"digest":digest});
        self.inner
            .spawn_record(spec, None, Some(request), None, Some(&effect))
            .await
    }

    pub(crate) async fn create_agent(
        &self,
        mut spec: AgentSpec,
        selection: crate::execution::ExecutionSelection,
    ) -> Result<AgentRecord, OrchError> {
        let _guard = self.inner.conversations.lock().await;
        if self
            .inner
            .state
            .lock()
            .unwrap()
            .agents
            .contains_key(&spec.name)
        {
            return Err(OrchError::NotAllowed(format!(
                "agent `{}` already exists",
                spec.name
            )));
        }
        if selection.explicit() {
            let inherited = if selection.model.is_none() {
                let resolved = self
                    .inner
                    .models
                    .resolve(&self.inner.lane(&spec.name), None)
                    .await?;
                let inherited = self.inner.models.selection_spec(&resolved);
                self.inner.models.close(&self.inner.lane(&spec.name)).await;
                Some(inherited)
            } else {
                None
            };
            spec.model = selection.apply(inherited.as_deref())?;
            if let Some(model) = &spec.model {
                self.inner.models.validate_selection(model).await?;
            }
        }
        self.inner.spawn_record(spec, None, None, None, None).await
    }

    pub fn agent(&self, name: &str) -> Option<AgentRecord> {
        let st = self.inner.state.lock().unwrap();
        st.agents.get(name).map(|a| a.record.clone())
    }

    /// The tools of the person's own conversation: create an agent, hand one a task, wait
    /// for tasks (see [`ChatTools`](crate::tools)). Add them to each turn with
    /// [`TurnOptions::tools`](ark_sdk::TurnOptions::tools); they name the agents of that moment.
    pub fn chat_tools(&self) -> ark_sdk::TurnTools {
        self.chat_tools_in(None, None)
    }

    /// [`chat_tools`](Self::chat_tools) for one conversation (`session`): what it schedules
    /// posts each run's result into it ([`TaskSpec::report_to`]).
    pub fn chat_tools_for(&self, session: &str) -> ark_sdk::TurnTools {
        self.chat_tools_in(Some(session.to_string()), None)
    }

    /// [`chat_tools_for`](Self::chat_tools_for) for a person in time zone `zone` (IANA, as
    /// their client says it): "at 9" is their 9, also on a runtime in another zone.
    pub fn chat_tools_in_zone(&self, session: &str, zone: Option<&str>) -> ark_sdk::TurnTools {
        self.chat_tools_in(Some(session.to_string()), zone.map(str::to_string))
    }

    fn chat_tools_in(&self, chat: Option<String>, zone: Option<String>) -> ark_sdk::TurnTools {
        ark_sdk::TurnTools {
            adapter: Arc::new(tools::ChatTools {
                orch: self.clone(),
                chat,
                zone,
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
            .tools_for(&self.inner.project.id)
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
        self.inner.start(&id)?;
        Ok(id)
    }

    /// Submit a client turn once, including retry after its receipt was lost.
    pub async fn submit_once(
        &self,
        agent: &str,
        spec: TaskSpec,
        request: &str,
    ) -> Result<String, OrchError> {
        let _guard = self.inner.conversations.lock().await;
        let key = format!("client:{}/{agent}/{request}", self.project().id);
        if let Some(task) = self
            .tasks()
            .into_iter()
            .find(|task| task.handoff_key.as_deref() == Some(&key))
        {
            if task.owner != agent
                || task.objective != spec.objective
                || task.context != spec.context
            {
                return Err(OrchError::NotAllowed(
                    "the request already names another message".into(),
                ));
            }
            return Ok(task.id);
        }
        let id = self
            .inner
            .create_run(agent, spec, Origin::External, None, Some(&key))?;
        self.inner.start(&id)?;
        Ok(id)
    }

    /// Run a task on `agent` and wait for it to end (done, failed or cancelled).
    pub async fn run(&self, agent: &str, spec: TaskSpec) -> Result<TaskRecord, OrchError> {
        let id = self.submit(agent, spec)?;
        self.wait(&id).await
    }

    /// Wait for a task to end.
    pub async fn wait(&self, task: &str) -> Result<TaskRecord, OrchError> {
        self.inner.wait_record(task).await
    }

    /// [`wait`](Self::wait) in a turn of `chat`: a task that reports to it gets its result there
    /// as this answer, not as a post too.
    /// The tasks handed off by any of `works` ([`TaskSpec::work`]), oldest first.
    pub fn tasks_of_work(&self, works: &[String]) -> Vec<TaskRecord> {
        self.tasks()
            .into_iter()
            .filter(|t| t.work.as_ref().is_some_and(|w| works.contains(w)))
            .collect()
    }

    /// A task still open that one of `works` handed to `agent`, now taken up by `work`: a
    /// turn that continues a stopped one joins its hand-off instead of making it twice.
    pub(crate) fn take_up(&self, agent: &str, works: &[String], work: &str) -> Option<TaskRecord> {
        let id = self
            .tasks_of_work(works)
            .into_iter()
            .find(|t| t.owner == agent && !t.state.is_final())?
            .id;
        self.inner
            .update_task(&id, |t| t.work = Some(work.to_string()))
    }

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
        self.answer_current(task, None, decision)
    }

    pub fn answer_question(
        &self,
        task: &str,
        question: &str,
        decision: ApprovalDecision,
    ) -> Result<(), OrchError> {
        self.answer_current(task, Some(question), decision)
    }

    fn answer_current(
        &self,
        task: &str,
        expected: Option<&str>,
        decision: ApprovalDecision,
    ) -> Result<(), OrchError> {
        self.inner.require_execution_control(task)?;
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
            if expected.is_some_and(|expected| q.id != expected) {
                return Err(OrchError::NotAllowed(
                    "that approval is no longer pending".into(),
                ));
            }
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

    /// Request cancellation of a task and every unfinished task it delegated. This returns
    /// when cancellation is requested; observe the final task state for completion.
    pub fn cancel(&self, task: &str) -> Result<(), OrchError> {
        self.inner.require_execution_control(task)?;
        if !self.inner.state.lock().unwrap().tasks.contains_key(task) {
            return Err(OrchError::UnknownTask(task.into()));
        }
        self.inner.cancel_tree(task, Stop::Cancel);
        Ok(())
    }

    /// Requests cancellation, then waits until the task and its delegated descendants finish.
    pub async fn cancel_and_wait(&self, task: &str) -> Result<(), OrchError> {
        self.cancel(task)?;
        let mut pending = vec![task.to_string()];
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            self.cancel(&id)?;
            let record = self.wait(&id).await?;
            pending.extend(record.children);
        }
        Ok(())
    }

    /// Stop scheduling work for `agent`. A task it is running stops at the next step and is
    /// suspended; it runs again, from the agent's working state, on [`resume`](Self::resume).
    pub fn suspend(&self, agent: &str) -> Result<(), OrchError> {
        self.inner.require_agent_control(agent)?;
        self.inner.suspend(agent)
    }

    pub fn resume(&self, agent: &str) -> Result<(), OrchError> {
        self.inner.require_agent_control(agent)?;
        self.inner.resume(agent)
    }

    /// Cancel everything `agent` has open, close its model session, and take no more work
    /// for it. Its records and store stay.
    pub async fn terminate(&self, agent: &str) -> Result<(), OrchError> {
        self.inner.require_agent_control(agent)?;
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
            self.inner.drop_schedule(&id)?;
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
        self.inner.require_agent_control(agent)?;
        self.inner.send(from, agent, text, None)
    }

    /// Send a note with a frozen, descriptive context snapshot.
    pub fn message_with_context(
        &self,
        from: &str,
        agent: &str,
        text: &str,
        context: &str,
    ) -> Result<bool, OrchError> {
        self.inner.require_agent_control(agent)?;
        self.inner
            .send_with_context(from, agent, text, None, context)
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
        self.schedule_with(agent, task, when, ScheduleChange::default())
    }

    /// [`schedule`](Self::schedule) with its policies set (`if_missed`, `overlap`, `retries`;
    /// the other fields of `opts` are ignored).
    pub fn schedule_with(
        &self,
        agent: &str,
        task: TaskSpec,
        when: When,
        opts: ScheduleChange,
    ) -> Result<ScheduleRecord, OrchError> {
        let when = when.normalized().map_err(OrchError::NotAllowed)?;
        match self.agent(agent) {
            None => return Err(OrchError::UnknownAgent(agent.into())),
            Some(a) if a.state == AgentState::Terminated => {
                return Err(OrchError::Terminated(agent.into()));
            }
            _ => {}
        }
        if task.objective.trim().is_empty() {
            return Err(OrchError::NotAllowed("a task needs an objective".into()));
        }
        let mut rec = ScheduleRecord::new(self.inner.project.id.as_str(), agent, task, when);
        if rec.next_ms.is_none() {
            return Err(OrchError::NotAllowed(format!(
                "{} has passed; give a time still to come",
                rec.says
            )));
        }
        opts.policies(&mut rec);
        let _one = self.inner.ticks.lock().unwrap();
        let rec = self.inner.save_schedule(&rec)?;
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

    /// The schedules kept (running or paused), soonest first, paused last.
    pub fn schedules(&self) -> Vec<ScheduleRecord> {
        let st = self.inner.state.lock().unwrap();
        let mut v: Vec<ScheduleRecord> = st.schedules.values().cloned().collect();
        v.sort_by_key(|s| (s.next_ms.is_none(), s.next_ms, s.created_ms));
        v
    }

    pub fn schedule_record(&self, id: &str) -> Option<ScheduleRecord> {
        self.inner.state.lock().unwrap().schedules.get(id).cloned()
    }

    /// Where a schedule is: scheduled, running, asking, retrying, succeeded, failed, paused.
    pub fn schedule_state(&self, s: &ScheduleRecord) -> crate::ScheduleState {
        if s.target.is_some() {
            use crate::{automation::RunPhase, ScheduleState};
            let last = match s.last_task.as_deref() {
                Some(id) => match self.automation_run(id) {
                    Ok(run) => Some(run),
                    Err(error) => {
                        tracing::error!(schedule=%s.id, %error, "could not read the scheduled execution state");
                        return ScheduleState::Failed;
                    }
                },
                None => None,
            };
            if let Some(run) = &last {
                match run.phase {
                    RunPhase::Pending | RunPhase::Running => return ScheduleState::Running,
                    RunPhase::WaitingApproval => return ScheduleState::Asking,
                    _ => {}
                }
            }
            if s.paused {
                return ScheduleState::Paused;
            }
            return match last.map(|run| run.phase) {
                Some(RunPhase::Completed) => ScheduleState::Succeeded,
                Some(RunPhase::Failed | RunPhase::Interrupted) => ScheduleState::Failed,
                _ => ScheduleState::Scheduled,
            };
        }
        let st = self.inner.state.lock().unwrap();
        let last = s
            .last_task
            .as_ref()
            .and_then(|t| st.tasks.get(t))
            .map(|t| &t.record);
        crate::ScheduleState::of(s, last)
    }

    /// Stop a schedule. A run already working goes on (cancel its task to stop it); one only
    /// waiting to try again is cancelled. The runs it made stay, with their results.
    pub fn unschedule(&self, id: &str) -> Result<ScheduleRecord, OrchError> {
        let rec = {
            let _one = self.inner.ticks.lock().unwrap();
            let gone = self.inner.drop_schedule(id)?;
            gone.ok_or_else(|| self.inner.no_schedule(id))?
        };
        self.inner.cancel_waiting_retry(&rec);
        Ok(rec)
    }

    /// [`unschedule`](Self::unschedule), and end the agent made for the schedule when no other
    /// schedule uses it and it has no open task.
    pub async fn stop_schedule(&self, id: &str) -> Result<ScheduleRecord, OrchError> {
        let rec = self.unschedule(id)?;
        let used = self.schedules().iter().any(|s| s.agent == rec.agent);
        let busy = self
            .agent(&rec.agent)
            .is_some_and(|a| a.current_task.is_some() || a.state == AgentState::Running);
        if rec.own_agent && !used && !busy {
            self.terminate(&rec.agent).await?;
        }
        Ok(rec)
    }

    /// Mark a schedule's agent as made for it ([`ScheduleRecord::own_agent`]).
    pub(crate) fn own_agent(&self, id: &str) {
        let _one = self.inner.ticks.lock().unwrap();
        let found = self.inner.state.lock().unwrap().schedules.get(id).cloned();
        if let Some(mut s) = found {
            s.own_agent = true;
            self.inner.keep_schedule(s);
        }
    }

    /// Keep a schedule but run nothing until [`resume_schedule`](Self::resume_schedule). A run
    /// already working goes on; one only waiting to try again is cancelled.
    pub fn pause_schedule(&self, id: &str) -> Result<ScheduleRecord, OrchError> {
        let rec = self.inner.change(id, |s| {
            s.paused = true;
            s.next_ms = None;
            s.admission_slot_ms = None;
            Ok(())
        })?;
        self.inner.cancel_waiting_retry(&rec);
        Ok(rec)
    }

    /// Run a paused schedule again from its next time; the times missed while it was paused do
    /// not run.
    pub fn resume_schedule(&self, id: &str) -> Result<ScheduleRecord, OrchError> {
        self.inner.change(id, |s| {
            s.paused = false;
            s.failures = 0;
            s.admission_error = None;
            s.admission_failures = 0;
            s.admission_retry_ms = None;
            s.next_ms = s
                .admission_slot_ms
                .take()
                .or_else(|| s.when.next_after(now_ms().max(s.last_ms.unwrap_or(0))));
            if s.next_ms.is_none() {
                return Err(OrchError::NotAllowed(format!(
                    "{} has passed; change its time to run it again",
                    s.says
                )));
            }
            Ok(())
        })
    }

    /// Change what a schedule runs or when. Runs already started keep what they were given.
    pub fn change_schedule(
        &self,
        id: &str,
        change: ScheduleChange,
    ) -> Result<ScheduleRecord, OrchError> {
        let when = match &change.when {
            Some(w) => Some(w.normalized().map_err(OrchError::NotAllowed)?),
            None => None,
        };
        self.inner.change(id, |s| {
            if let Some(o) = change.objective.as_deref().map(str::trim) {
                if o.is_empty() {
                    return Err(OrchError::NotAllowed("a task needs an objective".into()));
                }
                s.task.objective = o.to_string();
            }
            if let Some(c) = &change.context {
                s.task.context = c.trim().to_string();
            }
            if let Some(mut w) = when.clone() {
                // A new time of day is read in the zone the schedule had, unless it names one.
                if w.tz.is_none() && w.every_minutes.is_none() {
                    w.tz = s.when.tz.clone().or_else(crate::schedule::system_zone);
                }
                if w.next_after(now_ms()).is_none() {
                    return Err(OrchError::NotAllowed(format!(
                        "{} has passed; give a time still to come",
                        w.describe()
                    )));
                }
                s.says = w.describe();
                s.when = w;
                s.admission_slot_ms = None;
                s.admission_error = None;
                s.admission_failures = 0;
                s.admission_retry_ms = None;
                if !s.paused {
                    s.next_ms = s.when.next_after(now_ms());
                }
            }
            change.policies(s);
            Ok(())
        })
    }

    /// Start a run now, outside the schedule's times; its next time stays. Refused while a run
    /// is open, unless the schedule allows overlap. A paused schedule stays paused.
    pub async fn run_schedule_now(&self, id: &str) -> Result<String, OrchError> {
        self.run_schedule_now_inner(id, None).await
    }

    pub async fn run_schedule_now_with_key(
        &self,
        id: &str,
        request: &str,
    ) -> Result<String, OrchError> {
        crate::schedule::manual_receipt_id(self.project().id.as_str(), id, request)?;
        self.run_schedule_now_inner(id, Some(request)).await
    }

    async fn run_schedule_now_inner(
        &self,
        id: &str,
        request: Option<&str>,
    ) -> Result<String, OrchError> {
        if let Some(request) = request {
            if let Some(receipt) = self.schedule_run_receipt(id, request)? {
                return Ok(receipt.execution);
            }
        }
        let durable = self.inner.engine.store.transact_records::<_, OrchError>(
            self.project().id.as_str(),
            |tx| {
                tx.get_record(SCHEDULE, id)?
                    .map(serde_json::from_value::<ScheduleRecord>)
                    .transpose()
                    .map_err(Into::into)
            },
        )?;
        if durable
            .as_ref()
            .is_some_and(|record| record.target.is_some())
        {
            let generated = uuid::Uuid::new_v4().to_string();
            let run = self
                .inner
                .admit_manual_schedule_run(id, request.unwrap_or(&generated))?;
            self.execute_automation_run(&run.id)?;
            return Ok(run.id);
        }
        if let Some(request) = request {
            if let Some(receipt) = self.schedule_run_receipt(id, request)? {
                return Ok(receipt.execution);
            }
        }
        let mut record = durable.ok_or_else(|| self.inner.no_schedule(id))?;
        let open = self.inner.open_runs(id);
        if (open > 0 && !record.overlap) || open >= MAX_OPEN_RUNS {
            return Err(OrchError::NotAllowed(
                "a run is still under way; wait for it to end".into(),
            ));
        }
        let now = now_ms();
        self.inner
            .start_run(&mut record, now, now, false, request)
            .await
    }

    pub fn schedule_run_receipt(
        &self,
        id: &str,
        request: &str,
    ) -> Result<Option<crate::ScheduleRunReceipt>, OrchError> {
        let key = crate::schedule::manual_receipt_id(self.project().id.as_str(), id, request)?;
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let receipt: Option<crate::ScheduleRunReceipt> = tx
                    .get_record(crate::schedule::MANUAL_RECEIPT, &key)?
                    .map(serde_json::from_value)
                    .transpose()?;
                if receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.schedule != id || receipt.request != request)
                {
                    return Err(OrchError::NotAllowed(
                        "the manual trigger key names another execution".into(),
                    ));
                }
                Ok(receipt)
            })
    }

    /// A schedule's runs, newest first: each is a task with its state, result, error, outputs
    /// and times; its trace is [`task_trace`](Self::task_trace).
    pub fn schedule_runs(&self, id: &str, limit: usize) -> Vec<TaskRecord> {
        let st = self.inner.state.lock().unwrap();
        let mut v: Vec<TaskRecord> = st
            .tasks
            .values()
            .filter(|t| t.record.schedule.as_deref() == Some(id))
            .map(|t| t.record.clone())
            .collect();
        v.sort_by_key(|t| std::cmp::Reverse((t.created_ms, t.id.clone())));
        v.truncate(limit);
        v
    }

    /// Start every schedule due at `now_ms`; returns the tasks started. The orchestrator's
    /// timer calls this; an app with its own clock (or a test) can too.
    pub async fn tick_schedules(&self, now_ms: u64) -> Vec<String> {
        self.inner.tick_schedules(now_ms).await
    }

    /// Run [`tick_schedules`](Self::tick_schedules) every `every` while the orchestrator lives.
    /// [`OrchestratorBuilder::build`] starts one when it runs inside a tokio runtime.
    pub fn spawn_schedule_timer(&self, every: Duration) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { break };
                inner.tick_schedules(now_ms()).await;
                drop(inner);
                tokio::time::sleep(every).await;
            }
        })
    }

    // ---- the boards --------------------------------------------------------------------

    /// Every board's items in order: To do, In progress, Done (latest first). See
    /// [`crate::board`].
    pub fn board(&self) -> Vec<WorkItem> {
        let st = self.inner.state.lock().unwrap();
        board::ordered(st.work.values().cloned().collect())
    }

    /// The project's boards, the latest activity first.
    pub fn boards(&self) -> Vec<Board> {
        let st = self.inner.state.lock().unwrap();
        let items: Vec<WorkItem> = st.work.values().cloned().collect();
        board::latest_first(st.boards.values().cloned().collect(), &items)
    }

    /// The board for `goal`: the one with that goal, or a new one. `chat` is the conversation
    /// that asks for it, if one does.
    pub fn add_board(
        &self,
        by: &str,
        goal: &str,
        chat: Option<String>,
    ) -> Result<Board, OrchError> {
        self.inner.add_board(by, goal, chat)
    }

    /// Give a board another goal.
    pub fn change_board(&self, by: &str, id: &str, goal: &str) -> Result<Board, OrchError> {
        self.inner.change_board(by, id, goal)
    }

    /// Remove a board and its items. Tasks run for them stay as they are.
    pub fn remove_board(&self, id: &str) -> Result<Board, OrchError> {
        self.inner.remove_board(id)
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
    execution_lease: Option<ownership::ExecutionLease>,
    pub record: TaskRecord,
    execution_model: Option<String>,
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
    /// The project's boards and their items.
    pub boards: HashMap<String, Board>,
    pub work: HashMap<String, WorkItem>,
}

pub(crate) struct Inner {
    me: Weak<Inner>,
    pub engine: Arc<Engine>,
    base: AgentProfile,
    profiles: Vec<NamedProfile>,
    context_budget: Option<u32>,
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
    /// The folder each such task holds, from its first run until it ends (a pause included).
    holding: Mutex<HashMap<String, tokio::sync::OwnedMutexGuard<()>>>,
    pub max_parallel: usize,
    global: Arc<Semaphore>,
    accounts: Mutex<HashMap<String, Arc<Semaphore>>>,
    pub state: Mutex<State>,
    events: broadcast::Sender<OrchEvent>,
    stream: String,
    /// Held while schedules are ticked: one tick at a time.
    ticks: Arc<Mutex<()>>,
    conversations: tokio::sync::Mutex<()>,
    execution_token: String,
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

/// A run's failure that trying again cannot fix: no credit left, a project or action that is not
/// there. Anything else (the model unreachable or overloaded, a reply it could not get right, a
/// store or tool error) may pass, so a task with attempts left tries again.
fn final_error(e: &ark_runtime::AgentError) -> bool {
    use ark_runtime::AgentError as E;
    matches!(
        e,
        E::OutOfCredit { .. } | E::ProjectNotFound(_) | E::UnknownAction(_) | E::Plugin(_)
    )
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
    SavedResultFailed {
        answer: String,
        error: String,
    },
    Stopped(Stop),
    /// A failure another try may not hit (the model unreachable, a reply it could not get right).
    Failed(String),
    /// A failure trying again cannot fix (the account is out of credit).
    Fatal(String),
}

struct Final {
    _agent_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    state: TaskState,
    result: Option<String>,
    error: Option<String>,
}

impl Final {
    fn holding(mut self, guard: tokio::sync::OwnedMutexGuard<()>) -> Self {
        self._agent_guard = Some(guard);
        self
    }
    fn saved_result_failed(answer: String, error: String) -> Self {
        Self {
            _agent_guard: None,
            state: TaskState::Failed,
            result: Some(answer),
            error: Some(error),
        }
    }
    fn done(r: String) -> Self {
        Final {
            _agent_guard: None,
            state: TaskState::Done,
            result: Some(r),
            error: None,
        }
    }
    fn failed(e: impl Into<String>) -> Self {
        Final {
            _agent_guard: None,
            state: TaskState::Failed,
            result: None,
            error: Some(e.into()),
        }
    }
    fn cancelled(why: impl Into<String>) -> Self {
        Final {
            _agent_guard: None,
            state: TaskState::Cancelled,
            result: None,
            error: Some(why.into()),
        }
    }
    /// Not an end: the process is stopping and the task continues after the restart.
    fn left() -> Self {
        Final {
            _agent_guard: None,
            state: TaskState::Queued,
            result: None,
            error: None,
        }
    }
}

pub(crate) fn valid_name(n: &str) -> bool {
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

    fn save_schedule(&self, record: &ScheduleRecord) -> Result<ScheduleRecord, OrchError> {
        self.engine
            .store
            .transact_records(self.project.id.as_str(), |tx| {
                let mut next = record.clone();
                if let Some(value) = tx.get_record(SCHEDULE, &record.id)? {
                    let current: ScheduleRecord = serde_json::from_value(value)?;
                    if current.version != record.version || current.target != record.target {
                        return Err(OrchError::NotAllowed(
                            "the schedule changed; reload before saving".into(),
                        ));
                    }
                    next.version = next.version.checked_add(1).ok_or_else(|| {
                        OrchError::NotAllowed("the schedule version limit was reached".into())
                    })?;
                    tx.put_record(SCHEDULE, &record.id, &serde_json::to_value(&next)?)?;
                } else {
                    tx.insert_record(SCHEDULE, &record.id, &serde_json::to_value(&next)?)?;
                }
                Ok(next)
            })
    }

    /// Remove a schedule, from memory and the store under one lock, so a tick running at the
    /// same moment cannot put it back.
    fn drop_schedule(&self, id: &str) -> Result<Option<ScheduleRecord>, OrchError> {
        let mut state = self.state.lock().unwrap();
        let record = self
            .engine
            .store
            .transact_records(self.project.id.as_str(), |tx| {
                let record: Option<ScheduleRecord> = tx
                    .get_record(SCHEDULE, id)?
                    .map(serde_json::from_value)
                    .transpose()?;
                if let Some(record) = &record {
                    if record.target.is_some() {
                        return Err(OrchError::NotAllowed(
                            "stop this schedule through its automation".into(),
                        ));
                    }
                    tx.delete_record(SCHEDULE, id)?;
                }
                Ok::<_, OrchError>(record)
            })?;
        if let Some(record) = &record {
            state.schedules.remove(id);
            self.emit(EventKind::Unscheduled {
                schedule: record.id.clone(),
                agent: record.agent.clone(),
                says: record.says.clone(),
            });
        }
        Ok(record)
    }

    fn no_schedule(&self, id: &str) -> OrchError {
        let known: Vec<String> = self
            .state
            .lock()
            .unwrap()
            .schedules
            .keys()
            .cloned()
            .collect();
        OrchError::NotAllowed(if known.is_empty() {
            format!("no schedule `{id}`; nothing is scheduled")
        } else {
            format!("no schedule `{id}`; the schedules are {}", known.join(", "))
        })
    }

    /// Change a schedule under the timer's lock, so a tick never writes back a copy from
    /// before the change. Nothing changes when `f` refuses.
    fn change(
        &self,
        id: &str,
        f: impl FnOnce(&mut ScheduleRecord) -> Result<(), OrchError>,
    ) -> Result<ScheduleRecord, OrchError> {
        let _one = self.ticks.lock().unwrap();
        let unavailable = self.no_schedule(id);
        let mut state = self.state.lock().unwrap();
        let s = self
            .engine
            .store
            .transact_records(self.project.id.as_str(), |tx| {
                let mut s: ScheduleRecord =
                    serde_json::from_value(tx.get_record(SCHEDULE, id)?.ok_or(unavailable)?)?;
                if s.target.is_some() {
                    return Err(OrchError::NotAllowed(
                        "edit this schedule through its automation".into(),
                    ));
                }
                f(&mut s)?;
                s.version = s.version.checked_add(1).ok_or_else(|| {
                    OrchError::NotAllowed("the schedule version limit was reached".into())
                })?;
                tx.put_record(SCHEDULE, id, &serde_json::to_value(&s)?)?;
                Ok::<_, OrchError>(s)
            })?;
        state.schedules.insert(id.into(), s.clone());
        self.emit(EventKind::ScheduleChanged {
            schedule: s.id.clone(),
            agent: s.agent.clone(),
            says: s.says.clone(),
            paused: s.paused,
        });
        Ok(s)
    }

    /// Store a schedule and keep it in memory; one that will never run again is removed. Not
    /// for a schedule stopped meanwhile.
    fn keep_schedule(&self, s: ScheduleRecord) {
        let mut st = self.state.lock().unwrap();
        if !st.schedules.contains_key(&s.id) {
            return;
        }
        // A one-time schedule stays while its run is open, so it shows how that run goes.
        let running = s
            .last_task
            .as_ref()
            .and_then(|t| st.tasks.get(t))
            .is_some_and(|t| !t.record.state.is_final());
        if s.target.is_some() || s.kept() || running {
            match self.save_schedule(&s) {
                Ok(s) => {
                    st.schedules.insert(s.id.clone(), s);
                }
                Err(error) => {
                    tracing::error!(schedule=%s.id, %error, "could not retain schedule change")
                }
            }
        } else {
            let deleted = self.engine.store.transact_records::<_, OrchError>(
                self.project.id.as_str(),
                |tx| {
                    let current: ScheduleRecord =
                        serde_json::from_value(tx.get_record(SCHEDULE, &s.id)?.ok_or_else(
                            || OrchError::NotAllowed("the schedule is unavailable".into()),
                        )?)?;
                    if current.version != s.version || current.target != s.target {
                        return Err(OrchError::NotAllowed(
                            "the schedule changed before cleanup".into(),
                        ));
                    }
                    tx.delete_record(SCHEDULE, &s.id)?;
                    Ok(())
                },
            );
            match deleted {
                Ok(()) => {
                    st.schedules.remove(&s.id);
                }
                Err(error) => {
                    tracing::error!(schedule=%s.id, %error, "could not remove completed schedule")
                }
            }
        }
    }

    /// Runs of a schedule not finished yet.
    fn open_runs(&self, id: &str) -> usize {
        let st = self.state.lock().unwrap();
        st.tasks
            .values()
            .filter(|t| t.record.schedule.as_deref() == Some(id) && !t.record.state.is_final())
            .count()
    }

    /// Start one run of `s` for the time `slot`, at `now`. Its brief says which time it is for
    /// (and that it is late, when it is); it tries again after a failure that may pass.
    async fn start_run(
        &self,
        s: &mut ScheduleRecord,
        slot: u64,
        now: u64,
        due: bool,
        request: Option<&str>,
    ) -> Result<String, OrchError> {
        let mut spec = s.task.clone();
        let mut line = format!("This is the run scheduled for {}", s.when.label(slot));
        if let Some(tz) = s
            .when
            .tz
            .as_ref()
            .filter(|_| !line.ends_with(char::is_alphabetic))
        {
            line.push_str(&format!(" ({tz})"));
        }
        line.push('.');
        if now.saturating_sub(slot) > crate::schedule::LATE_MS {
            line.push_str(&format!(
                " It starts late, at {}: nothing ran at that time.",
                s.when.label(now)
            ));
        }
        spec.context = match spec.context.trim() {
            "" => line,
            c => format!("{line}\n{c}"),
        };
        spec.max_attempts = Some(1 + s.retries);
        spec.retry_delay_ms = Some(crate::schedule::RETRY_DELAY_MS);
        let request_key = if let Some(request) = request {
            crate::schedule::manual_receipt_id(self.project.id.as_str(), &s.id, request)?
        } else if due {
            format!("{}/{slot}", s.id)
        } else {
            format!("{}/{}", s.id, uuid::Uuid::new_v4())
        };
        let receipt = self
            .admit_work_context(
                WorkTarget::Agent {
                    name: s.agent.clone(),
                },
                spec,
                "schedule",
                &request_key,
                Some(admission::ScheduleAdmission {
                    expected: s,
                    slot,
                    now,
                    due,
                    request,
                }),
                None,
            )
            .await?;
        let id = receipt.task.clone().ok_or_else(|| {
            OrchError::NotAllowed("the scheduled execution has no agent task".into())
        })?;
        (Orchestrator { inner: self.arc() }).execute_work(&receipt.id)?;
        *s =
            self.engine
                .store
                .transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                    Ok(serde_json::from_value(
                        tx.get_record(SCHEDULE, &s.id)?.ok_or_else(|| {
                            OrchError::NotAllowed("the schedule was removed after admission".into())
                        })?,
                    )?)
                })?;
        self.state
            .lock()
            .unwrap()
            .schedules
            .insert(s.id.clone(), s.clone());
        self.prune_runs(&s.id);
        Ok(id)
    }

    /// Keep a schedule's newest [`RUNS_KEPT`] finished runs; older ones leave the store, so a
    /// schedule that runs every few minutes does not grow it without end.
    fn prune_runs(&self, id: &str) {
        let old: Vec<String> = {
            let st = self.state.lock().unwrap();
            let mut runs: Vec<&TaskRecord> = st
                .tasks
                .values()
                .map(|t| &t.record)
                .filter(|t| t.schedule.as_deref() == Some(id) && t.state.is_final())
                .filter(|t| t.item.is_none() && t.children.is_empty() && t.handoff_key.is_none())
                .collect();
            runs.sort_by_key(|t| std::cmp::Reverse(t.created_ms));
            runs.iter().skip(RUNS_KEPT).map(|t| t.id.clone()).collect()
        };
        for t in old {
            self.state.lock().unwrap().tasks.remove(&t);
            let _ = self.engine.store.delete_record(TASK, &t);
        }
    }

    /// Tell the chat a schedule reports to something about it (no result: a run let go, a pause).
    fn note(&self, s: &ScheduleRecord, text: String) {
        let Some(chat) = &s.task.report_to else {
            return;
        };
        let post = Post::new(&s.agent, clip(&s.task.objective, 80), text, false);
        match ark_runtime::session::post(&self.engine, chat, post.clone()) {
            Ok(_) => self.emit(EventKind::Posted {
                session: chat.clone(),
                task: s.last_task.clone().unwrap_or_default(),
                post,
            }),
            Err(e) => tracing::warn!(schedule = %s.id, "could not post into the chat: {e}"),
        }
    }

    /// A schedule's latest run ended: count a failure in a row (3 pause the schedule and say so
    /// in its chat), or clear the count; an ended one-time schedule goes.
    fn run_ended(&self, run: &TaskRecord) {
        let Some(id) = &run.schedule else { return };
        let _one = self.ticks.lock().unwrap();
        let found = self.state.lock().unwrap().schedules.get(id).cloned();
        let Some(mut s) = found else { return };
        if s.last_task.as_deref() != Some(run.id.as_str()) {
            return;
        }
        if s.counted.as_deref() != Some(run.id.as_str()) {
            s.counted = Some(run.id.clone());
            match run.state {
                TaskState::Failed => s.failures += 1,
                TaskState::Done => s.failures = 0,
                _ => {}
            }
        }
        let pause = s.failures >= MAX_FAILURES && !s.paused && s.active();
        if pause {
            self.pause_failing(&mut s);
        }
        self.keep_schedule(s);
    }

    /// Pause a schedule whose runs keep failing, and say so in its chat.
    fn pause_failing(&self, s: &mut ScheduleRecord) {
        s.paused = true;
        s.next_ms = None;
        self.note(
            s,
            format!(
                "Paused after {} runs in a row did not finish. Ask to resume it when the cause is fixed.",
                s.failures
            ),
        );
        self.emit(EventKind::ScheduleChanged {
            schedule: s.id.clone(),
            agent: s.agent.clone(),
            says: s.says.clone(),
            paused: true,
        });
    }

    /// A schedule stopped or paused: its run that only waits to try again is cancelled; one
    /// that works goes on.
    fn cancel_waiting_retry(&self, s: &ScheduleRecord) {
        let waiting = {
            let st = self.state.lock().unwrap();
            s.last_task
                .as_ref()
                .and_then(|t| st.tasks.get(t))
                .filter(|t| !t.record.state.is_final() && t.record.retry_at_ms.is_some())
                .map(|t| t.record.id.clone())
        };
        if let Some(t) = waiting {
            self.cancel_tree(&t, Stop::Cancel);
        }
    }

    /// Start the schedules due at `now`. One tick at a time, and every change to a schedule
    /// waits for it, so a pause or a change made meanwhile is never undone. See
    /// [`crate::schedule`] for the rules.
    async fn tick_schedules(&self, now: u64) -> Vec<String> {
        if let Err(error) = self.deliver_automation_reports() {
            tracing::error!(%error,"could not recover pending automation result deliveries");
        }
        let fresh = self
            .engine
            .store
            .list_records(SCHEDULE, Some(self.project.id.as_str()))
            .and_then(|values| {
                values
                    .into_iter()
                    .map(serde_json::from_value::<ScheduleRecord>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(Into::into)
            });
        match fresh {
            Ok(records) => {
                self.state.lock().unwrap().schedules = records
                    .into_iter()
                    .map(|record| (record.id.clone(), record))
                    .collect();
            }
            Err(error) => {
                tracing::error!(%error, "could not read schedules before dispatch");
                return vec![];
            }
        }
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
            if s.target.is_some() {
                match self.dispatch_due_automation(&s.id, now) {
                    Ok(Some(run)) => {
                        let id = run.id.clone();
                        match (Orchestrator { inner: self.arc() }).execute_automation_run(&id) {
                            Ok(_) => started.push(id),
                            Err(error) => {
                                tracing::error!(run=%id, %error, "could not dispatch admitted automation")
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::error!(schedule=%s.id, %error, "could not admit scheduled automation")
                    }
                }
                continue;
            }
            let Some(slot) = s.next_ms else { continue };
            let (last, open, made) = {
                let st = self.state.lock().unwrap();
                if !st.schedules.contains_key(&s.id) {
                    continue;
                }
                let last = s
                    .last_task
                    .as_ref()
                    .and_then(|t| st.tasks.get(t))
                    .map(|t| t.record.clone());
                let mine = st
                    .tasks
                    .values()
                    .filter(|t| t.record.schedule.as_deref() == Some(s.id.as_str()));
                let open = mine.clone().filter(|t| !t.record.state.is_final()).count();
                // A run for this time made before a crash, whose schedule was not saved.
                let made = mine
                    .clone()
                    .find(|t| t.record.slot == Some(slot))
                    .map(|t| t.record.id.clone());
                (last, open, made)
            };
            // Failures in a row: each finished run counts once, when the next one is due.
            if let Some(l) =
                last.filter(|l| l.state.is_final() && Some(&l.id) != s.counted.as_ref())
            {
                s.counted = Some(l.id.clone());
                if l.state == TaskState::Failed {
                    s.failures += 1;
                } else {
                    s.failures = 0;
                }
            }
            if let Some(id) = made {
                s.runs += 1;
                s.last_ms = Some(slot);
                s.last_task = Some(id);
            } else if s.failures >= MAX_FAILURES {
                self.pause_failing(&mut s);
                self.keep_schedule(s);
                continue;
            } else if now.saturating_sub(slot) > crate::schedule::LATE_MS
                && s.if_missed == IfMissed::Skip
            {
                s.missed += 1;
                if s.when.once {
                    self.note(
                        &s,
                        format!(
                            "Did not run: nothing was running at {}, and this schedule lets a missed run go.",
                            s.when.label(slot)
                        ),
                    );
                }
            } else if (open > 0 && !s.overlap) || open >= MAX_OPEN_RUNS {
                s.skipped += 1;
            } else {
                match self.start_run(&mut s, slot, now, true, None).await {
                    Ok(id) => {
                        started.push(id);
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(schedule = %s.id, %error, "a scheduled run could not start");
                        s.admission_slot_ms = Some(slot);
                        s.admission_error = Some(error.to_string());
                        s.admission_failures = s.admission_failures.saturating_add(1);
                        s.admission_retry_ms = Some(now.saturating_add(
                            crate::schedule::RETRY_DELAY_MS.saturating_mul(
                                1u64 << s.admission_failures.saturating_sub(1).min(6),
                            ),
                        ));
                        if s.admission_failures >= MAX_FAILURES {
                            s.paused = true;
                            s.next_ms = None;
                        }
                        self.keep_schedule(s);
                        continue;
                    }
                }
            }
            s.advance(slot, now);
            self.keep_schedule(s);
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
        let mut retired_leases = Vec::new();
        {
            let mut st = self.state.lock().unwrap();
            for v in store.list_records(AGENT, Some(self.project.id.as_str()))? {
                let Ok(mut r) = serde_json::from_value::<AgentRecord>(v) else {
                    continue;
                };
                let live = match &r.current_task {
                    Some(id) => store
                        .get_record(TASK, id)?
                        .map(serde_json::from_value::<TaskRecord>)
                        .transpose()?
                        .is_some_and(|task| {
                            task.execution_owner.is_some()
                                && task
                                    .pid
                                    .is_some_and(|pid| process::writer_alive(pid, task.pid_start))
                        }),
                    None => false,
                };
                if !live {
                    if matches!(r.state, AgentState::Running | AgentState::Waiting) {
                        r.state = AgentState::Idle;
                    }
                    r.current_task = None;
                }
                let (status, _) = watch::channel(r.state);
                let agent_lock = self.agent_execution_lock(&r.name)?;
                st.agents.insert(
                    r.name.clone(),
                    AgentRt {
                        record: r,
                        lock: agent_lock,
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
                let pending_result = r.submission.is_some()
                    && r.state != TaskState::Done
                    && store.get_record(results::RESULT, &r.id)?.is_some();
                let mut execution_lease = None;
                if r.run.is_none()
                    && r.submission.is_some()
                    && (!r.state.is_final() || pending_result)
                    && tokio::runtime::Handle::try_current().is_ok()
                {
                    let reserved = self.reserve_execution(&r.id)?;
                    r = reserved.0;
                    execution_lease = reserved.1;
                }
                let mut recovering = false;
                if r.state.is_final() {
                    if r.report_to.is_some() && !r.reported && reports(&r).is_some() {
                        late.push(r.id.clone());
                    }
                } else {
                    // Its result has not been anywhere yet.
                    r.reported = false;
                    let alive = if r.run.is_some() {
                        true
                    } else if r.submission.is_some() {
                        execution_lease.is_none()
                    } else {
                        r.pid
                            .is_some_and(|p| p != pid && process::writer_alive(p, r.pid_start))
                    };
                    if !alive {
                        if execution_lease.is_none() {
                            r.pid = None;
                            r.pid_start = None;
                        }
                        close_open_calls(&mut r);
                        carry_decisions(&mut r);
                        let asking = r.question.as_ref().is_some_and(Question::open);
                        let scope = st.agents.get(&r.owner).map(|a| a.record.scope.clone());
                        let unsure = match (scope, r.started_ms) {
                            (Some(scope), Some(started)) if !asking && !pending_result => {
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
                if r.state.is_final() {
                    retired_leases.extend(execution_lease.take());
                }
                let (done, _) = watch::channel(r.state.is_final());
                st.tasks.insert(
                    r.id.clone(),
                    TaskRt {
                        execution_lease,
                        execution_model: None,
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
                // Also an ended one-time schedule whose run is open: it goes when the run ends.
                // One whose run ended in a process that stopped before it said so goes now.
                if let Ok(s) = serde_json::from_value::<ScheduleRecord>(v) {
                    let open = s
                        .last_task
                        .as_ref()
                        .and_then(|t| st.tasks.get(t))
                        .is_some_and(|t| !t.record.state.is_final());
                    if s.target.is_some() || s.kept() || open {
                        st.schedules.insert(s.id.clone(), s);
                    } else {
                        let _ = store.delete_record(SCHEDULE, &s.id);
                    }
                }
            }
            for v in store.list_records(BOARD, Some(self.project.id.as_str()))? {
                if let Ok(b) = serde_json::from_value::<Board>(v) {
                    st.boards.insert(b.id.clone(), b);
                }
            }
            for v in store.list_records(WORK, Some(self.project.id.as_str()))? {
                if let Ok(w) = serde_json::from_value::<WorkItem>(v) {
                    st.work.insert(w.id.clone(), w);
                }
            }
            let (boards, items) =
                store.transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                    let mut data = work::WorkData::load(tx)?;
                    data.repair_boards(tx, self.project.id.as_str())?;
                    Ok((data.boards, data.items))
                })?;
            st.boards = boards;
            st.work = items;
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
            self.work_follows(&r)?;
            self.report_back(&r);
        }
        for id in late {
            self.report_late(&id);
        }
        for value in store.list_records(admission::SUBMISSION, Some(self.project.id.as_str()))? {
            let submission: admission::Submission = serde_json::from_value(value)?;
            if submission.receipt.turn.is_some() {
                let orchestrator = Orchestrator { inner: self.arc() };
                orchestrator.settle_general_work(&submission.receipt.id)?;
            }
        }
        drop(retired_leases);
        Ok(recover)
    }

    // ---- agents ------------------------------------------------------------------

    async fn spawn(&self, spec: AgentSpec) -> Result<AgentRecord, OrchError> {
        let _guard = self.conversations.lock().await;
        self.spawn_record(spec, None, None, None, None).await
    }

    async fn spawn_record(
        &self,
        spec: AgentSpec,
        creation: Option<(&str, &str)>,
        creation_request: Option<&str>,
        model_request: Option<&str>,
        chat_creation: Option<&serde_json::Value>,
    ) -> Result<AgentRecord, OrchError> {
        self.require_agent_control(&spec.name)?;
        if spec.title.chars().count() > 120 || spec.description.chars().count() > 500 {
            return Err(OrchError::NotAllowed("display metadata is too long".into()));
        }
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
                    creation_request: creation_request.map(str::to_string),
                    name: spec.name.clone(),
                    project: self.project.id.to_string(),
                    scope: scope.id.to_string(),
                    creation_task: creation.map(|(task, _)| task.to_string()),
                    creation_root: creation.map(|(_, root)| root.to_string()),
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
        let publication = (|| -> Result<(), OrchError> {
            let mut st = self.state.lock().unwrap();
            if model_request.is_some()
                && !st
                    .agents
                    .get(&spec.name)
                    .is_some_and(|a| a.record.state == AgentState::Idle)
            {
                return Err(OrchError::NotAllowed("the agent is busy".into()));
            }
            if let Some((task, _)) = creation {
                let active = st.tasks.get(task).is_some_and(|t| {
                    !t.record.state.is_final()
                        && !t.cancel.is_cancelled()
                        && t.stop.is_none()
                        && st.agents.get(&t.record.owner).is_some_and(|agent| {
                            !matches!(
                                agent.record.state,
                                AgentState::Terminated | AgentState::Suspended
                            )
                        })
                });
                if !active {
                    return Err(OrchError::NotAllowed(
                        "the creating turn is no longer active".into(),
                    ));
                }
            }
            let saved_record = serde_json::to_value(&record)?;
            let id = format!("{}/{}", self.project.id, record.name);
            if let Some(request) = model_request {
                let effect = serde_json::json!({ "agent": spec.name, "project": self.project.id, "model": spec.model });
                self.engine.store.put_records(&[
                    (AGENT, &id, self.project.id.as_str(), &saved_record),
                    (
                        "orch_model_change",
                        request,
                        self.project.id.as_str(),
                        &effect,
                    ),
                ])?;
            } else if let Some(effect) = chat_creation {
                let request = creation_request
                    .ok_or_else(|| OrchError::NotAllowed("missing creation request".into()))?;
                self.engine.store.put_records(&[
                    (AGENT, &id, self.project.id.as_str(), &saved_record),
                    (
                        "orch_chat_creation",
                        request,
                        self.project.id.as_str(),
                        effect,
                    ),
                ])?;
            } else {
                self.engine.store.put_record(
                    AGENT,
                    &id,
                    self.project.id.as_str(),
                    &saved_record,
                )?;
            }
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
                            lock: self.agent_execution_lock(&spec.name)?,
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
            Ok(())
        })();
        if let Err(error) = publication {
            if creation.is_some() {
                self.models.close(&self.lane(&spec.name)).await;
            }
            return Err(error);
        }

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
            Some(m)
                if !self.models.is_current(&m)
                    || (spec.model.is_none() && version != self.models.default_version()) =>
            {
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
        self.create_run(owner, spec, origin, parent, None)
    }

    fn create_run(
        &self,
        owner: &str,
        spec: TaskSpec,
        origin: Origin,
        parent: Option<&str>,
        handoff_key: Option<&str>,
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
        let mut record = TaskRecord::queued(
            id.clone(),
            self.project.id.to_string(),
            owner.to_string(),
            &spec,
            origin.clone(),
            spec.budget.clone().unwrap_or_default().or(&agent_budget),
            inputs,
        );
        record.handoff_key = handoff_key.map(str::to_string);
        record.parent = parent.map(str::to_string);
        {
            let mut st = self.state.lock().unwrap();
            if let (Some(_), Origin::Agent { agent, task }) = (handoff_key, &origin) {
                let active = st.tasks.get(task).is_some_and(|t| {
                    t.record.owner == *agent
                        && !t.record.state.is_final()
                        && !t.cancel.is_cancelled()
                        && t.stop.is_none()
                });
                if !active || !Self::contact_allowed_in(&st, agent, owner) {
                    return Err(OrchError::NotAllowed(
                        "the sending turn stopped or its contact changed".into(),
                    ));
                }
            }
            let linked = self.engine.store.transact_records::<_, OrchError>(
                self.project.id.as_str(),
                |tx| {
                    let linked = if let Some(item) = &spec.item {
                        let mut data = work::WorkData::load(tx)?;
                        let w = data.items.get_mut(item).ok_or_else(|| {
                            OrchError::NotAllowed(format!("no board item `{item}`"))
                        })?;
                        work::claimable(&data.tasks, w, owner)?;
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
                        tx.put_record(WORK, &w.id, &serde_json::to_value(&w)?)?;
                        Some(w.clone())
                    } else {
                        None
                    };
                    tx.insert_record(TASK, &record.id, &serde_json::to_value(&record)?)?;
                    Ok(linked)
                },
            )?;
            if let Some(w) = &linked {
                st.work.insert(w.id.clone(), w.clone());
            }
            let (done, _) = watch::channel(false);
            st.tasks.insert(
                id.clone(),
                TaskRt {
                    execution_lease: None,
                    execution_model: None,
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
            if let Some(parent) = parent.and_then(|p| st.tasks.get_mut(p)) {
                parent.record.children.push(id.clone());
                self.save_task(&parent.record);
            }
            if let Some(w) = linked {
                self.emit(EventKind::Work {
                    item: w,
                    by: owner.to_string(),
                });
            }
            self.emit(EventKind::TaskCreated {
                task: id.clone(),
                owner: owner.to_string(),
                origin,
                parent: parent.map(str::to_string),
                objective: clip(&spec.objective, 300),
                after: spec.after,
            });
        }
        Ok(id)
    }

    fn start(&self, id: &str) -> Result<(), OrchError> {
        let lease = {
            let mut state = self.state.lock().unwrap();
            let task = state
                .tasks
                .get_mut(id)
                .ok_or_else(|| OrchError::UnknownTask(id.into()))?;
            if task.record.submission.is_some() {
                match task.execution_lease.take() {
                    Some(lease) => Some(lease),
                    None => {
                        let (record, lease) = self.reserve_execution(id)?;
                        if lease.is_none() {
                            return Ok(());
                        }
                        task.record = record;
                        task.recovering |= task.record.attempts > 0;
                        lease
                    }
                }
            } else {
                None
            }
        };
        let me = self.arc();
        let id = id.to_string();
        tokio::spawn(async move {
            let _lease = lease;
            let f = match me.restore_work_result(&id) {
                Ok(Some(result)) => result,
                Ok(None) => match me.check_work_recovery(&id) {
                    Ok(()) => me.drive(&id).await,
                    Err(error) => Final::failed(error.to_string()),
                },
                Err(error) => Final::failed(format!("could not read the retained result: {error}")),
            };
            if f.state.is_final() {
                me.end_work(&id).await;
            }
            let task = me
                .state
                .lock()
                .unwrap()
                .tasks
                .get(&id)
                .map(|task| task.record.clone());
            if let Some(task) = task {
                match me.task_submission(&task) {
                    Ok(Some(submission)) => {
                        me.models
                            .close(&me.submission_lane(&submission, &task.owner))
                            .await
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::error!(task=%id, %error, "could not find the model lane to close")
                    }
                }
            }
            me.finish(&id, f).await;
            if let Err(error) = me.cleanup_work_result(&id) {
                tracing::error!(task = %id, %error, "could not settle the retained work result");
            }
        });
        Ok(())
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
        let mut st = self.state.lock().unwrap();
        let Some(t) = st.tasks.get_mut(id) else {
            return;
        };
        if t.record.state.is_final() {
            return;
        }
        match (&t.stop, &stop) {
            (Some(Stop::Cancel), _) => {}
            (None, _) | (_, Stop::Cancel) => t.stop = Some(stop),
            _ => {}
        }
        t.cancel.cancel();
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
        self.retry_record(id)?;
        self.start(id)?;
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
                    r = self.wait_record(d) => match r { Ok(record) => record, Err(error) => return Final::failed(error.to_string()) },
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
        // A wait to try again that a restart cut short goes on to its time.
        if !self.wait_to_retry(id).await {
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
            let models = match self.task_models_for(id, &owner).await {
                Ok(m) => m,
                Err(e) => return Final::failed(e.to_string()).holding(guard),
            };
            if !self.acquire(id, &models.account).await {
                return self.stopped_before_start(id).holding(guard);
            }
            if self.engine.is_stopping() {
                self.release(id).await;
                return Final::left().holding(guard);
            }
            let attempt = self.attempt(id, &owner, &models, resumed).await;
            // Answers carry only into the run that continues one the process stopped in.
            if !matches!(attempt, Attempt::Stopped(Stop::Restart)) {
                self.update_task(id, |t| t.decisions.iter_mut().for_each(|d| d.carry = false));
            }
            self.release(id).await;
            match attempt {
                Attempt::Done(answer) => return Final::done(answer).holding(guard),
                Attempt::SavedResultFailed { answer, error } => {
                    return Final::saved_result_failed(answer, error).holding(guard)
                }
                Attempt::Stopped(Stop::Suspend) => {
                    // A question it was asking is asked again when it runs again.
                    self.update_task(id, |t| {
                        t.state = TaskState::Suspended;
                        t.question = None;
                    });
                    continue;
                }
                Attempt::Stopped(Stop::Cancel) => {
                    return Final::cancelled("cancelled").holding(guard)
                }
                Attempt::Stopped(Stop::Restart) => return Final::left().holding(guard),
                Attempt::Stopped(Stop::Limit(l)) => {
                    return Final::failed(format!("budget exceeded: {l}")).holding(guard)
                }
                Attempt::Fatal(e) => return Final::failed(e).holding(guard),
                Attempt::Failed(e) => {
                    let (attempts, max, base) = {
                        let st = self.state.lock().unwrap();
                        let t = &st.tasks[id].record;
                        (t.attempts, t.max_attempts, t.retry_delay_ms)
                    };
                    if attempts >= max {
                        return Final::failed(e).holding(guard);
                    }
                    drop(guard);
                    let delay = Duration::from_millis(match base {
                        Some(b) => b.saturating_mul(1 << attempts.saturating_sub(1).min(6)),
                        None => 500 * (1 << attempts.min(6)),
                    });
                    self.emit(EventKind::TaskRetry {
                        task: id.to_string(),
                        attempt: attempts + 1,
                        error: clip(&e, 300),
                        delay_ms: delay.as_millis() as u64,
                    });
                    if delay >= Duration::from_secs(10) {
                        let at = now_ms() + delay.as_millis() as u64;
                        self.update_task(id, |t| t.retry_at_ms = Some(at));
                        if !self.wait_to_retry(id).await {
                            return self.stopped_before_start(id);
                        }
                    } else {
                        let cancel = self.cancel_token(id);
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {}
                            _ = cancel.cancelled() => return self.stopped_before_start(id),
                        }
                    }
                    // The next try continues this one: the changes it made are not made again.
                    resumed = true;
                }
            }
        }
    }

    /// Wait until the task's saved time to try again, queued with that time as the reason.
    /// False when it was cancelled meanwhile.
    async fn wait_to_retry(&self, id: &str) -> bool {
        let at = {
            let st = self.state.lock().unwrap();
            st.tasks.get(id).and_then(|t| t.record.retry_at_ms)
        };
        let Some(at) = at else { return true };
        let wait = at.saturating_sub(now_ms());
        if wait > 0 {
            let label = crate::schedule::local_label(at);
            self.queued(id, TaskState::Queued, format!("trying again at {label}"));
            let cancel = self.cancel_token(id);
            let deadline = tokio::time::sleep(Duration::from_millis(wait));
            tokio::pin!(deadline);
            let mut poll = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    _ = cancel.cancelled() => return false,
                    _ = poll.tick() => {
                        let schedule = self.state.lock().unwrap().tasks.get(id).and_then(|task| task.record.schedule.clone());
                        if let Some(schedule) = schedule {
                            match self.engine.store.get_record(SCHEDULE, &schedule).and_then(|value| value.map(serde_json::from_value::<ScheduleRecord>).transpose().map_err(Into::into)) {
                                Ok(Some(schedule)) if !schedule.paused => {},
                                Ok(_) => { self.stop(id, Stop::Cancel); return false; },
                                Err(error) => { self.stop(id, Stop::Limit(format!("could not read the schedule before retry: {error}"))); return false; },
                            }
                        }
                    }
                }
            }
        }
        self.update_task(id, |t| t.retry_at_ms = None);
        true
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
                    && !i.pid.is_some_and(|p| process::writer_alive(p, i.pid_start));
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
                let me = self.arc();
                let id = id.clone();
                tokio::spawn(async move {
                    me.finish(
                        &id,
                        Final::failed(format!("{INTERRUPTED}; its agent is gone")),
                    )
                    .await;
                });
                continue;
            }
            if !asking {
                self.emit(EventKind::TaskQueued {
                    task: id.clone(),
                    reason: RESTARTED.into(),
                });
            }
            if let Err(error) = self.start(id) {
                tracing::error!(task = %id, %error, "could not recover work execution");
            }
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
        // Between two tries (a retry's wait) the agent may run another task: its calls are not
        // this task's.
        let (started, others) = {
            let st = self.state.lock().unwrap();
            let Some(me) = st.tasks.get(id) else {
                return Vec::new();
            };
            let others: std::collections::HashSet<String> = st
                .tasks
                .values()
                .filter(|t| t.record.id != id && t.record.owner == me.record.owner)
                .flat_map(|t| t.record.tool_calls.iter().map(|c| c.invocation.clone()))
                .filter(|i| !i.is_empty())
                .collect();
            (me.record.started_ms, others)
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
            .filter(|i| !others.contains(i.id.as_str()))
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
        who.push_str(". Each request is one task; its brief names the requester.");
        if !delegates.is_empty() {
            who.push_str("\nYour contacts:\n");
            for (n, r) in delegates {
                who.push_str(&format!("- {n}: {}\n", if r.is_empty() { "-" } else { r }));
            }
            who.push_str("Colleagues receive your brief and artifacts, never this conversation. Use agents.start for an independent conversation and return immediately. Use agents.delegate only when this turn needs its result; delegate independent subtasks together.");
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
        let (task, mut agent, deps, children, inbox, cancel, roster, item) = {
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
            let inbox = if task.conversation.is_some() {
                Vec::new()
            } else {
                std::mem::take(&mut a.record.inbox)
            };
            worker = if task.conversation.is_some() {
                None
            } else {
                a.worker.clone()
            };
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
        if task.submission.is_some() {
            let durable = self
                .engine
                .store
                .get_record(AGENT, &format!("{}/{}", self.project.id, owner));
            match durable.and_then(|value| {
                value
                    .map(serde_json::from_value::<AgentRecord>)
                    .transpose()
                    .map_err(ark_store::StoreError::from)
            }) {
                Ok(Some(agent))
                    if matches!(agent.state, AgentState::Suspended | AgentState::Terminated) =>
                {
                    return Attempt::Fatal("the selected agent is unavailable".into())
                }
                Ok(Some(_)) => {}
                Ok(None) => return Attempt::Fatal("the selected agent is missing".into()),
                Err(error) => return Attempt::Fatal(error.to_string()),
            }
        }
        let scope = ProjectId(
            task.execution_scope
                .clone()
                .unwrap_or_else(|| agent.scope.clone()),
        );
        match self.prepare_submission(&task, &scope) {
            Ok(Some(spec)) => agent.spec = spec,
            Ok(None) => {}
            Err(error) => return Attempt::Fatal(error.to_string()),
        }
        if agent
            .spec
            .profile_name
            .as_ref()
            .is_some_and(|name| !self.profiles.iter().any(|profile| &profile.name == name))
        {
            return Attempt::Fatal("the agent profile is unavailable".into());
        }
        let spec = &agent.spec;
        let mut delegates: Vec<(String, String)> = roster
            .iter()
            .filter(|(n, _, s)| spec.may_delegate(n) && *s != AgentState::Terminated)
            .map(|(n, r, _)| (n.clone(), r.clone()))
            .collect();
        delegates.sort();
        let mut contacts: Vec<String> = roster
            .iter()
            .filter(|(n, _, s)| self.contact_allowed(owner, n) && *s != AgentState::Terminated)
            .map(|(n, _, _)| n.clone())
            .collect();
        contacts.sort();

        // Its tools: the allowed app actions, its agent tools, diagrams, rules.
        // The app's tools and the plugins connected now, as this agent may use them.
        let mut registry: Registry = self.engine.tools_for(&scope).filtered(|a| {
            spec.tools.iter().any(|p| action_matches(p, a))
                && (task.run.is_none() || !a.starts_with("agents."))
        });
        if task.run.is_none() {
            registry.register(Arc::new(AgentTools {
                orch: self.me.clone(),
                agent: owner.to_string(),
                task: id.to_string(),
                delegates: delegates.clone(),
                contacts,
                can_create: spec.tools.iter().any(|p| action_matches(p, tools::CREATE)),
            }));
        }
        if let Some(run_id) = &task.run {
            let run = match self.engine.store.transact_records::<_, OrchError>(
                self.project.id.as_str(),
                |tx| {
                    Ok(serde_json::from_value::<crate::automation::Run>(
                        tx.get_record(crate::automation::RUN, run_id)?
                            .ok_or_else(|| {
                                OrchError::NotAllowed("the execution is missing".into())
                            })?,
                    )?)
                },
            ) {
                Ok(run) => run,
                Err(error) => return Attempt::Fatal(error.to_string()),
            };
            let Some(step) = &task.execution_step else {
                return Attempt::Fatal("the task has no execution step".into());
            };
            registry.register(Arc::new(automation_children::RunTools {
                inner: self.arc(),
                run,
                step: step.clone(),
                parent: Some(task.id.clone()),
                capacity: None,
            }));
        }
        if spec.diagrams {
            registry.register(Arc::new(ark_tools::diagram::Diagrams));
        }
        // Its own web search and browser, as far as its `tools` allow.
        let owner_name = if self
            .state
            .lock()
            .unwrap()
            .tasks
            .get(id)
            .is_some_and(|task| task.record.submission.is_some())
        {
            format!(
                "{}/{owner}/work/{}",
                self.project.id,
                task.conversation.as_deref().unwrap_or(id)
            )
        } else {
            format!("{}/{owner}", self.project.id)
        };
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
        let engine = if task.submission.is_some() {
            self.engine.for_work(id).scoped_for(&scope, registry)
        } else {
            self.engine.scoped_for(&scope, registry)
        };
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
        // A run that continues an interrupted one gets the changes it already made, so it does
        // not make them twice.
        let done = if resumed && task.submission.is_some() {
            match self.engine.work_calls(&scope, id) {
                Ok(calls) => calls,
                Err(error) => return Attempt::Fatal(error.to_string()),
            }
        } else if resumed {
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
        let conversation_input = if task.conversation.is_some() {
            if task.context.trim().is_empty() {
                task.objective.clone()
            } else {
                format!("{}\n\n{}", task.objective, task.context)
            }
        } else {
            brief.clone()
        };

        let mut session = if let Some(conversation) = &task.conversation {
            let opened = if task.run.is_some() {
                Session::open_for_execution(&self.engine, conversation)
            } else {
                Session::open(&self.engine, conversation)
            };
            match opened {
                Ok(Some(session))
                    if session.record().project == self.project.id
                        || task.execution_scope.as_deref()
                            == Some(session.record().project.as_str()) =>
                {
                    session
                }
                Ok(Some(_)) => {
                    return Attempt::Fatal(
                        "the work conversation belongs to another project".into(),
                    )
                }
                Ok(None) => return Attempt::Fatal("the work conversation was deleted".into()),
                Err(error) => return Attempt::Fatal(error.to_string()),
            }
        } else {
            match agent
                .session
                .as_deref()
                .and_then(|s| Session::open(&self.engine, s).ok().flatten())
            {
                Some(s) => s,
                None => match Session::start(&self.engine, &scope) {
                    Ok(s) => s,
                    Err(e) => return Attempt::Failed(e.to_string()),
                },
            }
        };
        let steer = Steering::new();
        let Some(attempt_cancel) = cancel.child() else {
            return Attempt::Stopped(self.stop_reason(id).unwrap_or(Stop::Cancel));
        };
        let budget = &task.budget;
        if task.submission.is_some() {
            match session.execution_project(&self.engine) {
                Ok(context)
                    if context == scope
                        && session.record().work_item.as_ref() == task.item.as_ref() => {}
                Ok(_) => {
                    return Attempt::Fatal(
                        "the work conversation does not match its private context".into(),
                    )
                }
                Err(error) => return Attempt::Fatal(error.to_string()),
            }
        }
        let approvals = if let Some(run) = &task.run {
            match self.automation_approvals(run, id) {
                Ok(gate) => Some(gate),
                Err(error) => return Attempt::Fatal(error.to_string()),
            }
        } else {
            match &self.approvals {
                Some(gate) if !spec.allow_mutate => {
                    Some(Arc::new(ApprovalGate::new(Arc::new(OnBehalf {
                        orch: self.clone(),
                        agent: owner.to_string(),
                        task: id.to_string(),
                        gate: gate.clone(),
                    }))))
                }
                _ => None,
            }
        };
        let mut harness = Harness::new(models.router.clone_view())
            .max_steps(budget.max_steps.unwrap_or(self.max_steps))
            .budget_or_auto(self.context_budget)
            .planner(spec.planner.unwrap_or(self.planner))
            .critic(spec.critic.unwrap_or(self.critic))
            .allow_mutate(spec.allow_mutate)
            .approvals(approvals.clone())
            .cancel(Some(attempt_cancel.clone()))
            .stopping(Some(self.engine.stopping()))
            .steering(Some(steer.clone()))
            .conversation(session.conversation_digest())
            .rules(selection.clone())
            .skills(catalog)
            .done_before(done);

        if let Some(run) = &task.run {
            match self.run_guard(run, false).and_then(|guard| {
                guard.ensure_current()?;
                Ok(guard)
            }) {
                Ok(guard) => {
                    let Some(gate) = &approvals else {
                        return Attempt::Fatal("the execution has no approval route".into());
                    };
                    let guarded = engine.guarded(guard.clone());
                    match self
                        .resume_automation_actions(&guarded, &scope, id, gate, &guard, &cancel)
                        .await
                    {
                        Ok(calls) => harness = harness.guard(guard).done_before(calls),
                        Err(error) => return Attempt::Fatal(error.to_string()),
                    }
                }
                Err(error) => return Attempt::Fatal(error.to_string()),
            }
        }
        let session_id = session.id().to_string();
        if let Some(task) = self.state.lock().unwrap().tasks.get_mut(id) {
            task.execution_model = Some(self.models.selection_spec(models));
        }
        let attempt_no = {
            let rec = self.update_task(id, |t| {
                t.state = TaskState::Running;
                t.waiting_on = None;
                t.attempts += 1;
                t.started_ms.get_or_insert(now_ms());
                t.pid = Some(std::process::id());
                t.pid_start = ark_runtime::process::own_start();
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
            if task.conversation.is_none() {
                a.session = Some(session_id.clone());
            }
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
                for input in steer.take_undelivered_inputs() {
                    a.record.inbox.push(Mail {
                        from: "?".into(),
                        text: input.text,
                        context: input.context,
                        task: None,
                        at_ms: now_ms(),
                    });
                }
            }
            return ran;
        }
        if let Err(e) = session.begin_turn(&self.engine, &conversation_input) {
            if task.conversation.is_some() {
                if let Some(timer) = timer {
                    timer.abort();
                }
                return Attempt::Fatal(format!("could not record the turn start: {e}"));
            }
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
        let mut cleanup_delay = Duration::from_millis(250);
        while let Err(error) = attempt_cancel.wait_for_commit().await {
            tracing::error!(task = %id, %error, "could not finish pending task changes");
            self.queued(
                id,
                TaskState::Waiting,
                "Could not finish this task. Retrying.".into(),
            );
            tokio::time::sleep(cleanup_delay).await;
            cleanup_delay = (cleanup_delay * 2).min(Duration::from_secs(5));
        }
        let ran = started.elapsed().as_millis() as u64;
        self.update_task(id, |t| t.run_ms += ran);
        {
            let mut st = self.state.lock().unwrap();
            if let Some(a) = st.agents.get_mut(owner) {
                a.steering = None;
                // Messages that arrived too late for this task wait for the next one.
                for input in steer.take_undelivered_inputs() {
                    a.record.inbox.push(Mail {
                        from: "?".into(),
                        text: input.text,
                        context: input.context,
                        task: None,
                        at_ms: now_ms(),
                    });
                }
            }
        }
        self.collect_outputs(id, owner, &scope);
        match result {
            Ok(outcome) if !outcome.cancelled => {
                if task.submission.is_some() {
                    if let Err(error) =
                        self.save_work_result(id, &mut session, &conversation_input, &outcome)
                    {
                        return Attempt::SavedResultFailed { answer: outcome.answer, error: format!("the answer is retained but its conversation could not be saved: {error}") };
                    }
                } else if let Err(error) =
                    session.record_turn(&self.engine, &conversation_input, &outcome)
                {
                    tracing::warn!(task = %id, %error, "could not record the turn");
                }
                Attempt::Done(outcome.answer)
            }
            Ok(outcome) => {
                if task.submission.is_some() && !outcome.interrupted {
                    if let Err(error) =
                        self.save_work_result(id, &mut session, &conversation_input, &outcome)
                    {
                        return Attempt::SavedResultFailed {
                            answer: outcome.answer,
                            error: format!("the stopped turn could not be saved: {error}"),
                        };
                    }
                } else if let Err(error) =
                    session.record_turn(&self.engine, &conversation_input, &outcome)
                {
                    if task.submission.is_some() {
                        return Attempt::Fatal(format!(
                            "the interrupted turn could not be saved: {error}"
                        ));
                    }
                    tracing::warn!(task = %id, %error, "could not record the stopped turn");
                }
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
                if let Err(storage) =
                    session.fail_turn(&self.engine, &conversation_input, &e.to_string())
                {
                    if task.submission.is_some() {
                        return Attempt::Fatal(format!(
                            "{e}; the failed turn could not be saved: {storage}"
                        ));
                    }
                    tracing::warn!(task = %id, %storage, "could not record the failed turn");
                }
                match self.stop_reason(id) {
                    Some(s) => Attempt::Stopped(s),
                    None if self.engine.is_stopping() => Attempt::Stopped(Stop::Restart),
                    None if final_error(&e) => Attempt::Fatal(e.to_string()),
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

    async fn finish(&self, id: &str, mut f: Final) {
        if !f.state.is_final() {
            return self.leave(id);
        }
        let mut drained = false;
        let mut cleanup_delay = Duration::from_millis(250);
        let (rec, token) = loop {
            let pending = {
                let mut state = self.state.lock().unwrap();
                let Some(task) = state.tasks.get_mut(id) else {
                    return;
                };
                if task.cancel.is_cancelled() && !drained {
                    Err(task.cancel.clone())
                } else {
                    if matches!(task.stop, Some(Stop::Cancel)) {
                        f = Final::cancelled("cancelled");
                    } else if let Some(Stop::Limit(reason)) = &task.stop {
                        f = Final::failed(format!("budget exceeded: {reason}"));
                    }
                    task.record.state = f.state;
                    task.record.result = f.result.clone();
                    task.record.error = f.error.clone();
                    task.record.waiting_on = None;
                    task.record.finished_ms = Some(now_ms());
                    if task.record.submission.is_none() {
                        task.record.pid = None;
                        task.record.pid_start = None;
                    }
                    task.record.question = None;
                    task.answer = None;
                    Ok((task.record.clone(), task.cancel.clone()))
                }
            };
            match pending {
                Ok(value) => break value,
                Err(token) => match token.wait_for_commit().await {
                    Ok(()) => drained = true,
                    Err(error) => {
                        tracing::error!(task = %id, %error, "could not finish pending task changes");
                        self.queued(
                            id,
                            TaskState::Waiting,
                            "Could not finish this task. Retrying.".into(),
                        );
                        tokio::time::sleep(cleanup_delay).await;
                        cleanup_delay = (cleanup_delay * 2).min(Duration::from_secs(5));
                    }
                },
            }
        };
        self.save_task(&rec);
        token.release_drained_children();
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
        if let (Some(parent), true) = (&rec.parent, rec.run.is_none() && !rec.outputs.is_empty()) {
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
                        context: String::new(),
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
        if let Err(error) = self.work_follows(&rec) {
            tracing::error!(task = %rec.id, %error, "could not update its work item");
        }
        self.report_back(&rec);
        if rec.schedule.is_some() {
            self.run_ended(&rec);
        }
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
        match ark_runtime::session::post_once(&self.engine, chat, post.clone()) {
            Ok((_, created)) => {
                self.update_task(&t.id, |r| r.reported = true);
                if created {
                    self.emit(EventKind::Posted {
                        session: chat.clone(),
                        task: t.id.clone(),
                        post,
                    });
                }
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
        self.report_back(&rec);
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
            if t.submission.is_none() {
                t.pid = None;
                t.pid_start = None;
            }
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

    // ---- delegation and messages ---------------------------------------------------

    pub(crate) fn is_child(&self, parent: &str, task: &str) -> bool {
        let st = self.state.lock().unwrap();
        st.tasks
            .get(task)
            .is_some_and(|t| t.record.parent.as_deref() == Some(parent))
    }

    fn conversation_root(st: &State, task: &str) -> Result<String, OrchError> {
        let mut id = task;
        for _ in 0..=st.tasks.len() {
            let t = st
                .tasks
                .get(id)
                .ok_or_else(|| OrchError::UnknownTask(id.into()))?;
            match &t.record.origin {
                Origin::External => return Ok(id.to_string()),
                Origin::Agent { task, .. } => id = task,
            }
        }
        Err(OrchError::NotAllowed("cyclic conversation origin".into()))
    }

    pub(crate) fn contact_allowed(&self, from: &str, to: &str) -> bool {
        let st = self.state.lock().unwrap();
        Self::contact_allowed_in(&st, from, to)
    }

    fn contact_allowed_in(st: &State, from: &str, to: &str) -> bool {
        let Some(a) = st.agents.get(from) else {
            return false;
        };
        let Some(b) = st.agents.get(to) else {
            return false;
        };
        a.record.state != AgentState::Terminated
            && b.record.state != AgentState::Terminated
            && from != to
            && (a.record.spec.may_contact(to)
                || (b.record.creation_task.is_some()
                    && b.record.spec.parent.as_deref() == Some(from)))
    }

    pub(crate) async fn create_colleague(
        &self,
        from: &str,
        task: &str,
        name: &str,
        role: &str,
        instructions: &str,
        selection: crate::execution::ExecutionSelection,
    ) -> Result<AgentRecord, OrchError> {
        let request = format!(
            "colleague:{}",
            blake3::hash(&serde_json::to_vec(&(
                from,
                task,
                name,
                role,
                instructions,
                &selection
            ))?)
            .to_hex()
        );
        let _guard = self.conversations.lock().await;
        let (mut spec, root) = {
            let st = self.state.lock().unwrap();
            let source = st
                .agents
                .get(from)
                .ok_or_else(|| OrchError::UnknownAgent(from.into()))?;
            if !source
                .record
                .spec
                .tools
                .iter()
                .any(|p| action_matches(p, tools::CREATE))
            {
                return Err(OrchError::NotAllowed(
                    "this agent may not create agents".into(),
                ));
            }
            let t = st
                .tasks
                .get(task)
                .ok_or_else(|| OrchError::UnknownTask(task.into()))?;
            if t.record.owner != from
                || t.record.state.is_final()
                || t.cancel.is_cancelled()
                || t.stop.is_some()
            {
                return Err(OrchError::NotAllowed(
                    "the creating turn is no longer active".into(),
                ));
            }
            if let Some(existing) = st.agents.get(name) {
                if existing.record.creation_task.as_deref() == Some(task)
                    && existing.record.creation_request.as_deref() == Some(&request)
                {
                    return Ok(existing.record.clone());
                }
                return Err(OrchError::NotAllowed(format!(
                    "agent `{name}` already exists; use agents.start"
                )));
            }
            let root = Self::conversation_root(&st, task)?;
            let limit = t
                .record
                .budget
                .max_delegations
                .unwrap_or(8)
                .min(st.tasks[&root].record.budget.max_delegations.unwrap_or(8))
                .min(8) as usize;
            if st
                .agents
                .values()
                .filter(|a| a.record.creation_root.as_deref() == Some(&root))
                .count()
                >= limit
            {
                return Err(OrchError::NotAllowed(format!(
                    "this request may create {limit} agents"
                )));
            }
            let mut spec = source.record.spec.clone();
            let inherited = t
                .execution_model
                .clone()
                .or_else(|| {
                    source
                        .models
                        .as_ref()
                        .map(|models| self.models.selection_spec(models))
                })
                .or_else(|| source.record.spec.model.clone())
                .unwrap_or_else(|| source.record.model.clone());
            spec.model = selection.apply(Some(&inherited))?;
            (spec, root)
        };
        if selection.explicit() {
            if let Some(model) = &spec.model {
                self.models.validate_selection(model).await?;
            }
        }
        spec.name = name.to_string();
        spec.title.clear();
        spec.description.clear();
        spec.role = role.to_string();
        spec.instructions = instructions.to_string();
        spec.parent = Some(from.to_string());
        spec.allow_mutate = false;
        self.spawn_record(spec, Some((task, &root)), Some(&request), None, None)
            .await
    }

    pub(crate) async fn start_conversation(
        &self,
        from: &str,
        task: &str,
        to: &str,
        spec: TaskSpec,
    ) -> Result<String, OrchError> {
        let _guard = self.conversations.lock().await;
        if !self.contact_allowed(from, to) {
            return Err(OrchError::NotAllowed(format!(
                "{from} may not contact {to}"
            )));
        }
        let request = serde_json::to_vec(&(task, to, &spec.objective, &spec.context))?;
        let key = blake3::hash(&request).to_hex().to_string();
        {
            let st = self.state.lock().unwrap();
            if let Some(existing) = st
                .tasks
                .values()
                .find(|t| t.record.handoff_key.as_deref() == Some(&key))
            {
                if existing.record.owner != to
                    || existing.record.objective != spec.objective
                    || existing.record.context != spec.context
                {
                    return Err(OrchError::NotAllowed(
                        "the handoff key already names a different request".into(),
                    ));
                }
                return Ok(existing.record.id.clone());
            }
            let source = st
                .tasks
                .get(task)
                .ok_or_else(|| OrchError::UnknownTask(task.into()))?;
            if source.record.owner != from
                || source.record.state.is_final()
                || source.cancel.is_cancelled()
                || source.stop.is_some()
            {
                return Err(OrchError::NotAllowed(
                    "the sending turn is no longer active".into(),
                ));
            }
            let root = Self::conversation_root(&st, task)?;
            let limit = source
                .record
                .budget
                .max_delegations
                .unwrap_or(8)
                .min(st.tasks[&root].record.budget.max_delegations.unwrap_or(8))
                .min(8) as usize;
            let starts = st
                .tasks
                .values()
                .filter(|t| {
                    t.record.handoff_key.is_some()
                        && matches!(t.record.origin, Origin::Agent { .. })
                        && Self::conversation_root(&st, &t.record.id).is_ok_and(|r| r == root)
                })
                .count();
            if starts >= limit {
                return Err(OrchError::NotAllowed(format!(
                    "this request may start {limit} independent turns"
                )));
            }
        }
        let id = self.create_run(
            to,
            spec,
            Origin::Agent {
                agent: from.to_string(),
                task: task.to_string(),
            },
            None,
            Some(&key),
        )?;
        self.start(&id)?;
        Ok(id)
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
        self.start(&id)?;
        Ok(id)
    }

    /// Wait for tasks `parent` delegated, giving back its capacity meanwhile.
    pub(crate) async fn wait_children(
        this: &Arc<Inner>,
        parent: &str,
        ids: &[String],
    ) -> Result<Vec<TaskRecord>, OrchError> {
        this.park(parent, ids).await;
        let result = async {
            let mut out = Vec::new();
            for id in ids {
                out.push(this.wait_record(id).await?);
            }
            Ok(out)
        }
        .await;
        this.unpark(parent, ids).await;
        result
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
        self.send_with_context(from, to, text, task, "")
    }

    fn send_with_context(
        &self,
        from: &str,
        to: &str,
        text: &str,
        task: Option<&str>,
        context: &str,
    ) -> Result<bool, OrchError> {
        if text.trim().is_empty() {
            return Err(OrchError::NotAllowed("an empty message".into()));
        }
        if context.len() > ark_runtime::steer::MAX_STEERING_CONTEXT {
            return Err(OrchError::NotAllowed(
                "message context exceeds its size limit".into(),
            ));
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
            let queued: usize = a.record.inbox.iter().map(|mail| mail.context.len()).sum();
            let running = a.steering.as_ref().map_or(0, Steering::context_bytes);
            if queued.saturating_add(running).saturating_add(context.len())
                > ark_runtime::steer::MAX_QUEUED_CONTEXT
            {
                return Err(OrchError::NotAllowed(
                    "pending message context is full".into(),
                ));
            }
            match &a.steering {
                Some(s) => {
                    s.push_with_context(format!("Message from {from}: {text}"), context)
                        .map_err(OrchError::NotAllowed)?;
                    true
                }
                None => {
                    a.record.inbox.push(Mail {
                        from: from.to_string(),
                        text: text.to_string(),
                        context: context.to_string(),
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

#[cfg(test)]
#[path = "conversation_tests.rs"]
mod conversation_tests;
