//! An agent's own conversations ([`TaskSpec::thread`]). Each conversation runs its tasks one at
//! a time, and the conversations of one agent run at the same time. Identity, tools and memory
//! stay the agent's; the conversation, its working state and its model lane are its own.

use super::*;
use ark_runtime::session::SessionRecord;
use serde::{Deserialize, Serialize};

/// Hand-offs of one agent that may be open at once ([`Inner::work_later`]).
pub(super) const LATER_OPEN: usize = 3;

/// The lane of the agent's default conversation. Work and automations run on it too.
pub(super) const DEFAULT_LANE: &str = "default";

/// One conversation of an agent, as a list shows it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentThread {
    pub id: String,
    pub title: String,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
    pub turns: u32,
    /// The task running in it now.
    pub running: Option<String>,
    /// That task waits for the person's answer.
    pub asking: bool,
    /// The agent's default conversation ([`AgentRecord::session`]).
    pub default: bool,
}

/// The conversation of its own `task` runs in, when that is not the agent's default one
/// (`session`). Work and automation tasks always run on the default lane.
pub(super) fn own_thread<'a>(task: &'a TaskRecord, session: Option<&str>) -> Option<&'a str> {
    if task.conversation.is_some() || task.run.is_some() || task.submission.is_some() {
        return None;
    }
    task.thread.as_deref().filter(|t| Some(*t) != session)
}

/// Whether mail for `thread` is for the conversation `lane` (`None`: the default one).
pub(super) fn same_thread(thread: Option<&str>, lane: Option<&str>, session: Option<&str>) -> bool {
    thread.filter(|t| Some(*t) != session) == lane
}

/// A conversation of an agent: a session in its scope that is not Work's or an automation's.
fn is_conversation(record: &SessionRecord, scope: &str) -> bool {
    record.project.as_str() == scope
        && record.work_item.is_none()
        && !record.execution_managed
        && record.execution_scope.is_none()
}

/// The agent's state from the tasks it runs: a pause and an end stick; otherwise it works
/// while any task works, waits while all wait on others, and is idle without any.
pub(super) fn derived_state(current: AgentState, running: &[TaskState]) -> AgentState {
    if matches!(current, AgentState::Suspended | AgentState::Terminated) {
        return current;
    }
    let open: Vec<TaskState> = running
        .iter()
        .copied()
        .filter(|s| !s.is_final() && *s != TaskState::Suspended)
        .collect();
    if open.is_empty() {
        AgentState::Idle
    } else if open.iter().all(|s| *s == TaskState::Waiting) {
        AgentState::Waiting
    } else {
        AgentState::Running
    }
}

impl Orchestrator {
    /// Start a new, empty conversation with `agent`. Its tasks run in it with
    /// [`TaskSpec::thread`].
    pub fn start_thread(&self, agent: &str) -> Result<String, OrchError> {
        let record = self
            .agent(agent)
            .ok_or_else(|| OrchError::UnknownAgent(agent.into()))?;
        if record.state == AgentState::Terminated {
            return Err(OrchError::Terminated(agent.into()));
        }
        refuse_program(&record)?;
        let session = Session::start(&self.inner.engine, &ProjectId(record.scope))?;
        Ok(session.id().to_string())
    }

    /// The conversations of `agent` with turns, and its default one, newest first.
    pub fn threads(&self, agent: &str) -> Result<Vec<AgentThread>, OrchError> {
        agent_threads(&self.inner.engine, self.project().id.as_str(), agent)
    }
}

/// [`Orchestrator::threads`] read from the store alone: no orchestrator is opened for
/// `project`.
pub fn agent_threads(
    engine: &Engine,
    project: &str,
    agent: &str,
) -> Result<Vec<AgentThread>, OrchError> {
    let record: AgentRecord = engine
        .store
        .get_record(AGENT, &format!("{project}/{agent}"))?
        .map(serde_json::from_value)
        .transpose()?
        .filter(|r: &AgentRecord| r.project == project)
        .ok_or_else(|| OrchError::UnknownAgent(agent.into()))?;
    let mut running = Vec::new();
    for id in &record.running {
        if let Some(value) = engine.store.get_record(TASK, id)? {
            let task: TaskRecord = serde_json::from_value(value)?;
            if task.project == project
                && task.owner == record.name
                && !task.state.is_final()
                && task.conversation.is_none()
                && task.run.is_none()
                && task.submission.is_none()
            {
                running.push(task);
            }
        }
    }
    let session = record.session.as_deref();
    let tasks = person_tasks(engine, project)?;
    let objectives = first_objectives(&record, &tasks);
    let mut out = Vec::new();
    for s in Session::list(engine, Some(&ProjectId(record.scope.clone())))? {
        let id = s.id.as_str();
        let default = Some(id) == session;
        if !is_conversation(&s, &record.scope) || (s.turn_count == 0 && !default) || s.background {
            continue;
        }
        let task = running.iter().find(|t| match own_thread(t, session) {
            Some(thread) => thread == id,
            None => default,
        });
        out.push(AgentThread {
            id: id.to_string(),
            title: objectives
                .get(id)
                .cloned()
                .unwrap_or_else(|| s.title.clone()),
            updated_at: s.updated_at,
            turns: s.turn_count,
            running: task.map(|t| t.id.clone()),
            asking: task.is_some_and(|t| t.question.as_ref().is_some_and(Question::open)),
            default,
        });
    }
    Ok(out)
}

/// The person's own tasks in `project` (not Work's, an automation's or another agent's), oldest
/// first. Read once per project and passed to [`latest_thread_title`].
pub fn person_tasks(engine: &Engine, project: &str) -> Result<Vec<TaskRecord>, OrchError> {
    let mut tasks: Vec<TaskRecord> = Vec::new();
    for value in engine.store.list_records(TASK, Some(project))? {
        // A damaged task record names no chat; the session title stands.
        let Ok(task) = serde_json::from_value::<TaskRecord>(value) else {
            continue;
        };
        if task.project == project
            && matches!(task.origin, Origin::External)
            && task.conversation.is_none()
            && task.run.is_none()
            && task.submission.is_none()
        {
            tasks.push(task);
        }
    }
    tasks.sort_by_key(|t| t.created_ms);
    Ok(tasks)
}

/// What the person first asked in each of `agent`'s conversations, by session id. A turn of an
/// agent stores its rendered brief as the session title, so the objective names a chat better.
fn first_objectives(agent: &AgentRecord, tasks: &[TaskRecord]) -> HashMap<String, String> {
    let session = agent.session.as_deref();
    let mut out = HashMap::new();
    for task in tasks.iter().filter(|t| t.owner == agent.name) {
        let key = match own_thread(task, session) {
            Some(thread) => thread,
            None => match session {
                Some(default) => default,
                None => continue,
            },
        };
        let objective = task.objective.trim();
        if !objective.is_empty() {
            out.entry(key.to_string())
                .or_insert_with(|| clip_title(objective));
        }
    }
    out
}

fn clip_title(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    match line.char_indices().nth(60) {
        Some((at, _)) => format!("{}…", line[..at].trim_end()),
        None => line.to_string(),
    }
}

/// The title of `agent`'s latest conversation with turns, read from the store.
pub fn latest_thread_title(
    engine: &Engine,
    agent: &AgentRecord,
    tasks: &[TaskRecord],
) -> Result<Option<String>, OrchError> {
    let latest = Session::list(engine, Some(&ProjectId(agent.scope.clone())))?
        .into_iter()
        .find(|s| is_conversation(s, &agent.scope) && s.turn_count > 0);
    let Some(latest) = latest else {
        return Ok(None);
    };
    let mut objectives = first_objectives(agent, tasks);
    Ok(Some(
        objectives
            .remove(latest.id.as_str())
            .unwrap_or(latest.title),
    ))
}

fn refuse_program(agent: &AgentRecord) -> Result<(), OrchError> {
    if agent.on_program() {
        return Err(OrchError::NotAllowed(format!(
            "{} runs on an agent program, one conversation",
            agent.name
        )));
    }
    Ok(())
}

impl Inner {
    /// Hand the turn `task`'s work to a new conversation of the same agent, which posts its
    /// answer into the chat the turn is in as the agent's next message (ADR 0118). One hand-off
    /// per turn: asking again changes nothing and returns `true`. The new task keeps the turn's origin, so what
    /// asks in the turn asks in it (`sent_through_group`), and its conversation takes the
    /// chat's execution mode.
    pub(crate) async fn work_later(
        &self,
        agent: &str,
        task: &str,
        brief: &str,
    ) -> Result<bool, OrchError> {
        let _guard = self.conversations.lock().await;
        let key = blake3::hash(&serde_json::to_vec(&(task, "work.later"))?)
            .to_hex()
            .to_string();
        let (chat, origin, record) = {
            let st = self.state.lock().unwrap();
            if st
                .tasks
                .values()
                .any(|t| t.record.handoff_key.as_deref() == Some(&key))
            {
                return Ok(true);
            }
            let source = st
                .tasks
                .get(task)
                .ok_or_else(|| OrchError::UnknownTask(task.into()))?;
            let t = &source.record;
            if t.owner != agent
                || t.state.is_final()
                || source.cancel.is_cancelled()
                || source.stop.is_some()
            {
                return Err(OrchError::NotAllowed(
                    "this turn is no longer active".into(),
                ));
            }
            if !t.is_chat_turn() {
                return Err(OrchError::NotAllowed(
                    "only a chat turn can answer later".into(),
                ));
            }
            let open = st
                .tasks
                .values()
                .filter(|o| o.record.owner == agent && o.record.reply && !o.record.state.is_final())
                .count();
            if open >= LATER_OPEN {
                return Err(OrchError::NotAllowed(format!(
                    "{LATER_OPEN} answers are already on their way; wait for one"
                )));
            }
            let agent_record = st
                .agents
                .get(agent)
                .ok_or_else(|| OrchError::UnknownAgent(agent.into()))?
                .record
                .clone();
            let chat = t
                .thread
                .clone()
                .or_else(|| agent_record.session.clone())
                .ok_or_else(|| OrchError::NotAllowed("this turn has no chat".into()))?;
            (chat, t.origin.clone(), agent_record)
        };
        refuse_program(&record)?;
        let mut lane = Session::start(&self.engine, &ProjectId(record.scope.clone()))?;
        lane.set_background(&self.engine)?;
        if let Some(asking) = Session::open(&self.engine, &chat)? {
            let mode = asking.execution_mode();
            if mode != ark_core::ExecutionMode::Auto {
                lane.set_execution_mode(&self.engine, lane.execution_mode_revision(), mode)?;
            }
        }
        let mut spec = TaskSpec::new(brief)
            .work(task)
            .report_to(&chat)
            .thread(lane.id().as_str());
        spec.reply = true;
        let id = self.create_run(agent, spec, origin, None, Some(&key))?;
        // A Stop on the turn that came before the hand-off was made means no work: the task
        // ends unstarted. A Stop after it does not reach the work; the person stops that from
        // the agent's tile (ADR 0118).
        let stopped = {
            let st = self.state.lock().unwrap();
            st.tasks
                .get(task)
                .is_none_or(|t| t.cancel.is_cancelled() || t.stop.is_some())
        };
        if stopped {
            self.cancel_tree(&id, Stop::Cancel);
            return Err(OrchError::NotAllowed("this turn was stopped".into()));
        }
        self.start(&id)?;
        Ok(false)
    }

    /// Refuse `thread` for `agent` unless it is one of the agent's own conversations.
    pub(super) fn check_thread(&self, agent: &AgentRecord, thread: &str) -> Result<(), OrchError> {
        refuse_program(agent)?;
        let record = self
            .engine
            .store
            .get_session(thread)?
            .map(serde_json::from_value::<SessionRecord>)
            .transpose()?;
        match record {
            Some(r) if is_conversation(&r, &agent.scope) => Ok(()),
            _ => Err(OrchError::NotAllowed(format!(
                "that conversation is not one of {}'s",
                agent.name
            ))),
        }
    }

    /// The execution lane of task `id`: its own conversation, or [`DEFAULT_LANE`].
    pub(super) fn task_lane(&self, id: &str) -> String {
        let st = self.state.lock().unwrap();
        let Some(task) = st.tasks.get(id) else {
            return DEFAULT_LANE.into();
        };
        let session = st
            .agents
            .get(&task.record.owner)
            .and_then(|a| a.record.session.as_deref());
        match own_thread(&task.record, session) {
            Some(thread) => format!("thread/{thread}"),
            None => DEFAULT_LANE.into(),
        }
    }

    /// The conversation of its own task `id` runs in, when not the default one.
    pub(super) fn task_thread(&self, id: &str) -> Option<String> {
        let st = self.state.lock().unwrap();
        let task = st.tasks.get(id)?;
        let session = st
            .agents
            .get(&task.record.owner)
            .and_then(|a| a.record.session.as_deref());
        own_thread(&task.record, session).map(str::to_string)
    }

    /// The model lane of `agent`'s conversation `thread`.
    pub(super) fn thread_lane(&self, agent: &str, thread: &str) -> String {
        format!("{}/{agent}/thread/{thread}", self.project.id)
    }

    /// The models of `agent` for one task in its conversation `thread`, on that conversation's
    /// own lane and the agent's model selection. Never the agent's cached models: a vendor
    /// session belongs to one conversation.
    pub(super) async fn thread_models(
        &self,
        agent: &str,
        thread: &str,
    ) -> Result<Arc<AgentModels>, OrchError> {
        let record = {
            let st = self.state.lock().unwrap();
            st.agents
                .get(agent)
                .map(|a| a.record.clone())
                .ok_or_else(|| OrchError::UnknownAgent(agent.into()))?
        };
        refuse_program(&record)?;
        let lane = self.thread_lane(agent, thread);
        let models = self
            .models
            .resolve(&lane, record.spec.model.as_deref())
            .await?;
        if let Some(s) = &models.session {
            let weak = self.me.clone();
            let name = agent.to_string();
            s.set_listener(Arc::new(move |e| {
                if let Some(inner) = weak.upgrade() {
                    inner.emit(EventKind::Session {
                        agent: name.clone(),
                        event: e.clone(),
                    });
                }
            }));
        }
        let mut st = self.state.lock().unwrap();
        if let Some(a) = st.agents.get_mut(agent) {
            a.threads.insert(lane);
        }
        Ok(Arc::new(models))
    }

    /// Release the model lane of `agent`'s conversation `thread` (its task ended).
    pub(super) async fn close_thread_models(&self, agent: &str, thread: &str) {
        let lane = self.thread_lane(agent, thread);
        let open = {
            let mut st = self.state.lock().unwrap();
            st.agents
                .get_mut(agent)
                .is_some_and(|a| a.threads.remove(&lane))
        };
        if open {
            self.models.close(&lane).await;
            self.release_owner(&lane).await;
        }
    }

    /// Free what each per-agent source (its browser pages) holds for the owner `lane`.
    async fn release_owner(&self, lane: &str) {
        for source in &self.per_agent {
            source.release(lane).await;
        }
    }

    /// Release every conversation lane `agent` holds open.
    pub(super) async fn close_all_thread_models(&self, agent: &str) {
        let lanes: Vec<String> = {
            let mut st = self.state.lock().unwrap();
            st.agents
                .get_mut(agent)
                .map(|a| a.threads.drain().collect())
                .unwrap_or_default()
        };
        for lane in lanes {
            self.models.close(&lane).await;
            self.release_owner(&lane).await;
        }
    }

    /// Set `agent`'s state from the tasks it runs ([`derived_state`]).
    pub(super) fn sync_agent_state(&self, agent: &str) {
        self.settle_agent_state(agent, false);
    }

    /// [`Inner::sync_agent_state`]; `unpause` ends a suspend first.
    pub(super) fn settle_agent_state(&self, agent: &str, unpause: bool) {
        let next = {
            let st = self.state.lock().unwrap();
            let Some(a) = st.agents.get(agent) else {
                return;
            };
            let running: Vec<TaskState> = a
                .record
                .running
                .iter()
                .filter_map(|id| st.tasks.get(id).map(|t| t.record.state))
                .collect();
            let current = match a.record.state {
                AgentState::Suspended if unpause => AgentState::Idle,
                state => state,
            };
            derived_state(current, &running)
        };
        self.set_agent_state(agent, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_state_follows_its_running_tasks() {
        use AgentState as A;
        use TaskState as T;
        assert_eq!(derived_state(A::Running, &[]), A::Idle);
        assert_eq!(derived_state(A::Idle, &[T::Running]), A::Running);
        assert_eq!(
            derived_state(A::Idle, &[T::Waiting, T::Running]),
            A::Running
        );
        assert_eq!(
            derived_state(A::Running, &[T::Waiting, T::Waiting]),
            A::Waiting
        );
        assert_eq!(derived_state(A::Running, &[T::Done, T::Suspended]), A::Idle);
        assert_eq!(derived_state(A::Suspended, &[T::Running]), A::Suspended);
        assert_eq!(derived_state(A::Terminated, &[T::Running]), A::Terminated);
    }

    #[test]
    fn an_agent_on_a_program_keeps_one_conversation() {
        let record = |model: &str| -> AgentRecord {
            serde_json::from_value(serde_json::json!({
                "name": "w", "project": "p", "scope": "s", "state": "idle", "created_ms": 1,
                "spec": AgentSpec::new("w").model(model),
            }))
            .unwrap()
        };
        let program = record("agent:coder");
        assert!(program.on_program());
        let refused = refuse_program(&program).unwrap_err().to_string();
        assert!(refused.contains("one conversation"), "{refused}");
        assert!(refuse_program(&record("m")).is_ok());
    }

    #[test]
    fn a_thread_is_its_own_lane_unless_it_is_the_default_conversation() {
        let mut t = TaskRecord::queued(
            "t-1".into(),
            "p".into(),
            "w".into(),
            &TaskSpec::new("x").thread("s-2"),
            Origin::External,
            Default::default(),
            Vec::new(),
        );
        assert_eq!(own_thread(&t, Some("s-1")), Some("s-2"));
        assert_eq!(own_thread(&t, Some("s-2")), None);
        t.conversation = Some("work".into());
        assert_eq!(
            own_thread(&t, Some("s-1")),
            None,
            "Work stays on the default lane"
        );
        assert!(same_thread(Some("s-1"), None, Some("s-1")));
        assert!(same_thread(None, None, Some("s-1")));
        assert!(!same_thread(Some("s-2"), None, Some("s-1")));
        assert!(same_thread(Some("s-2"), Some("s-2"), Some("s-1")));
    }
}
