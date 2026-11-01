use super::*;
use ark_core::{Artifact, SessionId};
use ark_runtime::session::{CarryPolicy, SessionRecord};
use serde::{Deserialize, Serialize};

pub(super) const SUBMISSION: &str = "orch_work_submission";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkTarget {
    General { model: Option<String> },
    Agent { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkReceipt {
    pub id: String,
    pub project: String,
    pub origin: String,
    pub request: String,
    pub digest: String,
    pub conversation: String,
    pub work: String,
    pub task: Option<String>,
    #[serde(default)]
    pub turn: Option<u32>,
    pub model: String,
    pub created_ms: u64,
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub parent: Option<WorkRunRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Submission {
    pub receipt: WorkReceipt,
    pub target: WorkTarget,
    pub spec: TaskSpec,
    pub artifacts: Vec<Artifact>,
    pub rules: Vec<RuleBinding>,
    #[serde(default)]
    pub agent: Option<AgentSpec>,
}

pub(super) struct ScheduleAdmission<'a> {
    pub expected: &'a ScheduleRecord,
    pub slot: u64,
    pub now: u64,
    pub due: bool,
    pub request: Option<&'a str>,
}

fn identity(project: &str, origin: &str, request: &str) -> Result<String, OrchError> {
    if origin.is_empty() || origin.len() > 64 || request.is_empty() || request.len() > 256 {
        return Err(OrchError::NotAllowed(
            "a submission needs an origin and a bounded request key".into(),
        ));
    }
    Ok(format!(
        "wr-{}",
        blake3::hash(&serde_json::to_vec(&(project, origin, request))?).to_hex()
    ))
}

fn check(submission: &Submission, project: &str, digest: &str) -> Result<(), OrchError> {
    if submission.receipt.project != project || submission.receipt.digest != digest {
        return Err(OrchError::NotAllowed(
            "the submission key already names another request".into(),
        ));
    }
    Ok(())
}

impl Orchestrator {
    /// Atomically admit a new work conversation, or recover its original receipt.
    /// Admission does not execute a model turn.
    pub async fn admit_work(
        &self,
        target: WorkTarget,
        spec: TaskSpec,
        origin: &str,
        request: &str,
    ) -> Result<WorkReceipt, OrchError> {
        self.inner.admit_work(target, spec, origin, request).await
    }

    pub async fn admit_work_followup(
        &self,
        target: WorkTarget,
        spec: TaskSpec,
        origin: &str,
        request: &str,
        receipt: &str,
    ) -> Result<WorkReceipt, OrchError> {
        self.inner
            .admit_work_context(target, spec, origin, request, None, Some(receipt))
            .await
    }

    /// Admit a followup, returning `None` while its conversation or automation is busy.
    /// Rejections remain errors and do not create an admission.
    pub async fn try_admit_work_followup(
        &self,
        target: WorkTarget,
        spec: TaskSpec,
        origin: &str,
        request: &str,
        receipt: &str,
    ) -> Result<Option<WorkReceipt>, OrchError> {
        self.inner
            .try_admit_work_context(target, spec, origin, request, None, Some(receipt))
            .await
    }

    pub fn work_receipt_by_id(&self, id: &str) -> Result<Option<WorkReceipt>, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                tx.get_record(SUBMISSION, id)?
                    .map(|value| {
                        serde_json::from_value::<Submission>(value)
                            .map(|submission| submission.receipt)
                            .map_err(Into::into)
                    })
                    .transpose()
            })
    }

    pub fn work_receipt(
        &self,
        origin: &str,
        request: &str,
    ) -> Result<Option<WorkReceipt>, OrchError> {
        let id = identity(self.project().id.as_str(), origin, request)?;
        self.work_receipt_by_id(&id)
    }

    pub fn execute_work(&self, receipt_id: &str) -> Result<WorkReceipt, OrchError> {
        let mut state = self.inner.state.lock().unwrap();
        let (submission, task, agent) =
            self.inner
                .engine
                .store
                .transact_records(self.project().id.as_str(), |tx| {
                    let value = tx.get_record(SUBMISSION, receipt_id)?.ok_or_else(|| {
                        OrchError::NotAllowed("no work submission with that identity".into())
                    })?;
                    let submission: Submission = serde_json::from_value(value)?;
                    let id = submission.receipt.task.as_deref().ok_or_else(|| {
                        OrchError::NotAllowed("General runs in its conversation".into())
                    })?;
                    let task: TaskRecord = serde_json::from_value(
                        tx.get_record(TASK, id)?
                            .ok_or_else(|| OrchError::UnknownTask(id.into()))?,
                    )?;
                    let agent: AgentRecord = serde_json::from_value(
                        tx.get_record(AGENT, &format!("{}/{}", self.project().id, task.owner))?
                            .ok_or_else(|| OrchError::UnknownAgent(task.owner.clone()))?,
                    )?;
                    if agent.state == AgentState::Terminated {
                        return Err(OrchError::Terminated(agent.name));
                    }
                    if agent.state == AgentState::Suspended {
                        return Err(OrchError::NotAllowed("the selected agent is paused".into()));
                    }
                    if let Some(id) = &task.run {
                        let run: crate::automation::Run = serde_json::from_value(
                            tx.get_record(crate::automation::RUN, id)?.ok_or_else(|| {
                                OrchError::NotAllowed("the execution is unavailable".into())
                            })?,
                        )?;
                        if run
                            .owner
                            .as_ref()
                            .is_none_or(|owner| owner.token != self.inner.execution_token)
                            || !matches!(
                                run.phase,
                                crate::automation::RunPhase::Running
                                    | crate::automation::RunPhase::WaitingApproval
                            )
                            || run.cancel_requested
                            || !run.steps.iter().any(|step| {
                                (step.receipt.as_ref() == Some(&task.id)
                                    || task.execution_step.as_deref() == Some(&step.id))
                                    && step.phase == crate::automation::StepPhase::Running
                            })
                        {
                            return Err(OrchError::NotAllowed(
                                "this task is outside the current automation step".into(),
                            ));
                        }
                    }
                    if task.submission.as_deref() != Some(receipt_id)
                        || task.conversation.as_deref()
                            != Some(submission.receipt.conversation.as_str())
                    {
                        return Err(OrchError::NotAllowed(
                            "the task no longer matches this submission".into(),
                        ));
                    }
                    Ok::<_, OrchError>((submission, task, agent))
                })?;
        let agent_lock = self.inner.agent_execution_lock(&agent.name)?;
        state
            .agents
            .entry(agent.name.clone())
            .and_modify(|current| {
                if current.steering.is_none() {
                    current.record.state = agent.state;
                    current.record.current_task = agent.current_task.clone();
                    current.status.send_replace(agent.state);
                }
            })
            .or_insert_with(|| {
                let (status, _) = watch::channel(agent.state);
                AgentRt {
                    record: agent,
                    lock: agent_lock,
                    status,
                    steering: None,
                    models: None,
                    worker: None,
                    default_version: 0,
                    step: None,
                }
            });
        state.tasks.entry(task.id.clone()).or_insert_with(|| {
            let (done, _) = watch::channel(task.state.is_final());
            TaskRt {
                execution_lease: None,
                execution_model: None,
                record: task.clone(),
                cancel: CancelToken::new(),
                stop: None,
                done,
                slots: Default::default(),
                waiting_for: Vec::new(),
                delegations: 0,
                answer: None,
                answered: Default::default(),
                recovering: false,
            }
        });
        drop(state);
        self.inner.start(&task.id)?;
        Ok(submission.receipt)
    }

    pub fn archive_work(
        &self,
        user: &str,
        id: &str,
        archived: bool,
    ) -> Result<WorkItem, OrchError> {
        if user.is_empty() || user.len() > 256 {
            return Err(OrchError::NotAllowed("a user identity is required".into()));
        }
        self.inner.archive_work(user, id, archived)
    }
}

impl Inner {
    pub(super) async fn wait_record(&self, id: &str) -> Result<TaskRecord, OrchError> {
        let record =
            self.engine
                .store
                .transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                    tx.get_record(TASK, id)?
                        .map(serde_json::from_value::<TaskRecord>)
                        .transpose()?
                        .ok_or_else(|| OrchError::UnknownTask(id.into()))
                })?;
        if record.submission.is_some() {
            return self.wait_admitted(id).await;
        }
        if !self.state.lock().unwrap().tasks.contains_key(id) {
            return Err(OrchError::UnknownTask(id.into()));
        }
        Ok(self.wait_final(id).await)
    }

    pub(super) async fn wait_admitted(&self, id: &str) -> Result<TaskRecord, OrchError> {
        loop {
            let record = self.engine.store.transact_records::<_, OrchError>(
                self.project.id.as_str(),
                |tx| {
                    let value = tx
                        .get_record(TASK, id)?
                        .ok_or_else(|| OrchError::UnknownTask(id.into()))?;
                    Ok(serde_json::from_value::<TaskRecord>(value)?)
                },
            )?;
            let alive = record.execution_owner.is_some()
                && record
                    .pid
                    .is_some_and(|pid| process::writer_alive(pid, record.pid_start));
            if record.state.is_final() && !alive {
                return Ok(record);
            }
            if !alive && record.attempts > 0 {
                return Err(OrchError::NotAllowed(
                    "work execution stopped; reconnect to resume the saved task".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub(super) fn task_submission(
        &self,
        task: &TaskRecord,
    ) -> Result<Option<Submission>, OrchError> {
        let Some(id) = &task.submission else {
            return Ok(None);
        };
        let value = self
            .engine
            .store
            .get_record(SUBMISSION, id)?
            .ok_or_else(|| OrchError::NotAllowed("the work submission is missing".into()))?;
        let submission: Submission = serde_json::from_value(value)?;
        if submission.receipt.project != self.project.id.as_str()
            || submission.receipt.task.as_deref() != Some(task.id.as_str())
            || task.conversation.as_deref() != Some(submission.receipt.conversation.as_str())
            || submission.agent.as_ref().map(|agent| agent.name.as_str())
                != Some(task.owner.as_str())
        {
            return Err(OrchError::NotAllowed(
                "the work submission does not match this task".into(),
            ));
        }
        Ok(Some(submission))
    }

    pub(super) fn submission_lane(&self, submission: &Submission, owner: &str) -> String {
        let context = if submission.receipt.parent.is_some() {
            &submission.receipt.conversation
        } else {
            &submission.receipt.work
        };
        format!("{}/work/{context}/{owner}", self.project.id)
    }

    pub(super) async fn task_models_for(
        &self,
        id: &str,
        owner: &str,
    ) -> Result<Arc<AgentModels>, OrchError> {
        let task = self
            .state
            .lock()
            .unwrap()
            .tasks
            .get(id)
            .map(|task| task.record.clone())
            .ok_or_else(|| OrchError::UnknownTask(id.into()))?;
        let Some(submission) = self.task_submission(&task)? else {
            return self.models_for(owner).await;
        };
        let lane = self.submission_lane(&submission, owner);
        let models = self
            .models
            .resolve(&lane, Some(&submission.receipt.model))
            .await?;
        if self.models.selection_spec(&models) != submission.receipt.model {
            self.models.close(&lane).await;
            return Err(OrchError::NotAllowed(
                "the admitted model selection is unavailable".into(),
            ));
        }
        if task.run.is_some() {
            models.require_host_tools()?;
        }
        Ok(Arc::new(models))
    }

    pub(super) fn prepare_submission(
        &self,
        task: &TaskRecord,
        scope: &ProjectId,
    ) -> Result<Option<AgentSpec>, OrchError> {
        let Some(submission) = self.task_submission(task)? else {
            return Ok(None);
        };
        for artifact in &submission.artifacts {
            self.engine.store.put_artifact(scope, artifact)?;
        }
        let book = RuleBook::new(&self.engine.store);
        for binding in submission.rules {
            book.bind(&RuleBinding {
                ruleset: binding.ruleset,
                version: binding.version,
                scope: RuleScope::Task(task.id.clone()),
            })?;
        }
        Ok(submission.agent)
    }

    async fn admit_work(
        &self,
        target: WorkTarget,
        spec: TaskSpec,
        origin: &str,
        request: &str,
    ) -> Result<WorkReceipt, OrchError> {
        self.admit_work_context(target, spec, origin, request, None, None)
            .await
    }

    pub(super) async fn admit_work_context(
        &self,
        target: WorkTarget,
        spec: TaskSpec,
        origin: &str,
        request: &str,
        scheduled: Option<ScheduleAdmission<'_>>,
        selected_receipt: Option<&str>,
    ) -> Result<WorkReceipt, OrchError> {
        self.try_admit_work_context(target, spec, origin, request, scheduled, selected_receipt)
            .await?
            .ok_or_else(|| OrchError::NotAllowed("this work is busy; wait for its answer".into()))
    }

    async fn try_admit_work_context(
        &self,
        target: WorkTarget,
        spec: TaskSpec,
        origin: &str,
        request: &str,
        scheduled: Option<ScheduleAdmission<'_>>,
        selected_receipt: Option<&str>,
    ) -> Result<Option<WorkReceipt>, OrchError> {
        let project = self.project.id.as_str();
        let id = identity(project, origin, request)?;
        let digest = if let Some(receipt) = selected_receipt {
            format!(
                "v2:{}",
                blake3::hash(&serde_json::to_vec(&(&target, &spec, receipt))?).to_hex()
            )
        } else {
            format!(
                "v1:{}",
                blake3::hash(&serde_json::to_vec(&(&target, &spec))?).to_hex()
            )
        };
        if let Some(value) = self.engine.store.get_record(SUBMISSION, &id)? {
            let submission: Submission = serde_json::from_value(value)?;
            if scheduled.is_none() {
                check(&submission, project, &digest)?;
            }
            return Ok(Some(submission.receipt));
        }
        if spec.objective.trim().is_empty() {
            return Err(OrchError::NotAllowed("a task needs an objective".into()));
        }
        let agent = match &target {
            WorkTarget::General { .. } => None,
            WorkTarget::Agent { name } => {
                let value = self
                    .engine
                    .store
                    .get_record(AGENT, &format!("{project}/{name}"))?
                    .ok_or_else(|| OrchError::UnknownAgent(name.clone()))?;
                let agent: AgentRecord = serde_json::from_value(value)?;
                if agent.state == AgentState::Terminated {
                    return Err(OrchError::Terminated(name.clone()));
                }
                if self.worker_for(agent.spec.model.as_deref())?.is_some() {
                    return Err(OrchError::NotAllowed(
                        "this agent program cannot yet open an isolated work conversation".into(),
                    ));
                }
                Some(agent)
            }
        };
        if matches!(target, WorkTarget::General { .. })
            && (!spec.after.is_empty()
                || !spec.rules.is_empty()
                || spec.budget.is_some()
                || spec.max_attempts.is_some()
                || spec.retry_delay_ms.is_some()
                || spec.work.is_some())
        {
            return Err(OrchError::NotAllowed(
                "General accepts conversation messages; task policies require an agent".into(),
            ));
        }
        let selected = match &target {
            WorkTarget::General { model } => model.as_deref(),
            WorkTarget::Agent { .. } => agent.as_ref().and_then(|a| a.spec.model.as_deref()),
        };
        let preflight_lane = format!("{project}/work/{id}");
        let models = self.models.resolve(&preflight_lane, selected).await?;
        let model = self.models.selection_spec(&models);
        self.models.close(&preflight_lane).await;
        drop(models);
        let mut artifacts = Vec::new();
        let mut references = spec.artifacts.clone();
        for reference in ark_runtime::session::artifact_references(&format!(
            "{}\n{}",
            spec.objective, spec.context
        ))? {
            if !references.contains(&reference) {
                references.push(reference);
            }
        }
        for artifact in &references {
            artifacts.push(
                self.engine
                    .store
                    .resolve_artifact(&self.project.id, artifact)?
                    .ok_or_else(|| {
                        OrchError::NotAllowed(format!("no artifact `{artifact}` to pass on"))
                    })?,
            );
        }
        let book = RuleBook::new(&self.engine.store);
        let mut rules = Vec::new();
        for binding in &spec.rules {
            let set = book
                .get(&binding.ruleset, binding.version)?
                .ok_or_else(|| {
                    OrchError::NotAllowed(format!("no rule set `{}`", binding.ruleset))
                })?;
            rules.push(RuleBinding {
                ruleset: set.name,
                version: Some(set.version),
                scope: binding.scope.clone(),
            });
        }
        let mut state = self.state.lock().unwrap();
        let (submission, created, item, board, task) =
            self.engine.store.transact_records(project, |tx| {
                if let Some(value) = tx.get_record(SUBMISSION, &id)? {
                    let existing: Submission = serde_json::from_value(value)?;
                    if scheduled.is_none() { check(&existing, project, &digest)?; }
                    return Ok::<_, OrchError>((Some(existing), false, None, None, None));
                }
                if let Some(agent) = &agent {
                    let current = tx
                        .get_record(AGENT, &format!("{project}/{}", agent.name))?
                        .ok_or_else(|| OrchError::UnknownAgent(agent.name.clone()))?;
                    let current: AgentRecord = serde_json::from_value(current)?;
                    if current.state == AgentState::Terminated
                        || serde_json::to_value(&current.spec)?
                            != serde_json::to_value(&agent.spec)?
                    {
                        return Err(OrchError::NotAllowed(
                            "the agent changed; review it before submitting again".into(),
                        ));
                    }
                }
                for dependency in &spec.after {
                    if tx.get_record(TASK, dependency)?.is_none() {
                        return Err(OrchError::UnknownTask(dependency.clone()));
                    }
                }
                let mut data = work::WorkData::load(tx)?;
                let mut new_board = None;
                let mut item = if let Some(item) = &spec.item {
                    data.items
                        .get(item)
                        .cloned()
                        .ok_or_else(|| OrchError::NotAllowed(format!("no board item `{item}`")))?
                } else {
                    let board = match board::latest_first(
                        data.boards.values().cloned().collect(),
                        &data.items.values().cloned().collect::<Vec<_>>(),
                    )
                    .into_iter()
                    .next()
                    {
                        Some(board) => board,
                        None => {
                            let board = data.ensure_board(tx, project, PERSON, FIRST_GOAL, None)?;
                            new_board = Some(board.clone());
                            board
                        }
                    };
                    WorkItem::new(project, &board.id, PERSON, spec.objective.trim())
                };
                if let Some(run) = &item.run {
                    let run: crate::automation::Run = serde_json::from_value(
                        tx.get_record(crate::automation::RUN, run)?.ok_or_else(|| {
                            OrchError::NotAllowed("the automation execution is missing".into())
                        })?,
                    )?;
                    if !run.phase.is_final() {
                        return Ok((None, false, None, None, None));
                    }
                }
                let selected_conversation = if let Some(receipt) = selected_receipt {
                    let selected: Submission = serde_json::from_value(tx.get_record(SUBMISSION, receipt)?.ok_or_else(|| OrchError::NotAllowed("the selected execution is unavailable".into()))?)?;
                    if selected.receipt.work != item.id || selected.target != target || selected.receipt.parent.is_some() {
                        return Err(OrchError::NotAllowed("the selected execution belongs to another work item or target".into()));
                    }
                    Some(selected.receipt.conversation)
                } else { None };
                let conversation = if let Some(conversation) = selected_conversation.as_ref().or_else(|| item.conversation.as_ref().filter(|_| scheduled.is_none())) {
                    if tx.get_session(conversation)?.is_none() {
                        return Err(OrchError::NotAllowed(
                            "this work's conversation was deleted; start a new work item".into(),
                        ));
                    }
                    conversation.clone()
                } else {
                    let mut header = SessionRecord::empty(
                        SessionId::new(),
                        self.project.id.clone(),
                        CarryPolicy::default(),
                        Some(model.clone()),
                    );
                    header.title = clip(spec.objective.trim(), 120);
                    header.work_item = Some(item.id.clone());
                    tx.create_session(header.id.as_str(), &serde_json::to_value(&header)?)?;
                    let conversation = header.id.to_string();
                    if item.conversation.is_none() { item.conversation = Some(conversation.clone()); }
                    conversation
                };
                if scheduled.is_none() && spec
                    .report_to
                    .as_ref()
                    .is_some_and(|chat| chat != &conversation)
                {
                    return Err(OrchError::NotAllowed(
                        "a work submission cannot report into another conversation".into(),
                    ));
                }
                let mut header: SessionRecord =
                    serde_json::from_value(tx.get_session(&conversation)?.ok_or_else(|| {
                        OrchError::NotAllowed("the work conversation was deleted".into())
                    })?)?;
                if header.execution_scope.is_none() {
                    let scope = Project::new(&header.title, "").with_parent(self.project.id.clone());
                    tx.create_execution_project(&scope)?;
                    tx.insert_record("session_execution_scope", scope.id.as_str(), &serde_json::json!({"session":conversation,"work":item.id}))?;
                    header.execution_scope = Some(scope.id);
                    tx.put_session(&conversation, &serde_json::to_value(&header)?)?;
                }
                if header.work_item.as_ref().is_some_and(|id| id != &item.id) {
                    return Err(OrchError::NotAllowed(
                        "this conversation belongs to another work item".into(),
                    ));
                }
                if header.work_item.is_none() {
                    let mut bound = header.clone();
                    bound.work_item = Some(item.id.clone());
                    tx.put_session(&conversation, &serde_json::to_value(&bound)?)?;
                }
                let slot_id = format!("{conversation}/{}", header.turn_count);
                if tx.get_record(general::GENERAL_SLOT, &slot_id)?.is_some() {
                    return Ok((None, false, None, None, None));
                }
                let turn = if agent.is_none() {
                    if item
                        .tasks
                        .iter()
                        .filter_map(|id| data.tasks.get(id))
                        .any(|task| !task.state.is_final())
                    {
                        return Ok((None, false, None, None, None));
                    }
                    tx.insert_record(
                        general::GENERAL_SLOT,
                        &slot_id,
                        &serde_json::to_value(general::GeneralSlot {
                            receipt: id.clone(),
                            owner: None,
                        })?,
                    )?;
                    Some(header.turn_count)
                } else {
                    None
                };
                let task = if let Some(agent) = &agent {
                    if work::blocking_task(&data.tasks, &item, &agent.name).is_some() {
                        return Ok((None, false, None, None, None));
                    }
                    let task_id = format!("t-{}", uuid::Uuid::new_v4().simple());
                    let inputs = artifacts
                        .iter()
                        .map(|a| ArtifactRef {
                            id: a.id.to_string(),
                            name: a.name.clone(),
                            kind: a.kind.as_tag(),
                        })
                        .collect();
                    let mut task = TaskRecord::queued(
                        task_id,
                        project.to_string(),
                        agent.name.clone(),
                        &spec,
                        Origin::External,
                        spec.budget
                            .clone()
                            .unwrap_or_default()
                            .or(&agent.spec.budget),
                        inputs,
                    );
                    task.item = Some(item.id.clone());
                    task.conversation = Some(conversation.clone());
                    task.execution_scope = header.execution_scope.as_ref().map(|scope| scope.to_string());
                    task.submission = Some(id.clone());
                    task.report_to = if scheduled.is_some() { spec.report_to.clone() } else { None };
                    if let Some(schedule) = &scheduled {
                        if let Some(chat) = &task.report_to {
                            if tx.get_session(chat)?.is_none() { return Err(OrchError::NotAllowed("the schedule's result conversation is unavailable in this project".into())); }
                        }
                        let expected = schedule.expected;
                        let mut current: ScheduleRecord = serde_json::from_value(tx.get_record(SCHEDULE, &expected.id)?.ok_or_else(|| OrchError::NotAllowed("the schedule was removed before admission".into()))?)?;
                        if current.version != expected.version || current.target.is_some() || current.agent != agent.name || schedule.due && (current.paused || current.next_ms != Some(schedule.slot)) {
                            return Err(OrchError::NotAllowed("the scheduled target or occurrence changed before admission".into()));
                        }
                        let open = tx.records_matching(TASK, "schedule", &current.id)?.into_iter().map(serde_json::from_value::<TaskRecord>).collect::<Result<Vec<_>, _>>()?.into_iter().filter(|task| !task.state.is_final()).count();
                        if open >= MAX_OPEN_RUNS || open > 0 && !current.overlap { return Err(OrchError::NotAllowed("a previous scheduled execution is still open".into())); }
                        let admitted_at = now_ms().max(schedule.now);
                        if schedule.due && current.if_missed == IfMissed::Skip && admitted_at.saturating_sub(schedule.slot) > crate::schedule::LATE_MS { return Err(OrchError::NotAllowed("the occurrence expired while preparing its model".into())); }
                        if let Some(last) = &current.last_task {
                            if current.counted.as_ref() != Some(last) {
                                if let Some(value) = tx.get_record(TASK, last)? {
                                    let last: TaskRecord = serde_json::from_value(value)?;
                                    if last.state.is_final() {
                                        current.counted = Some(last.id);
                                        current.failures = if last.state == TaskState::Failed { current.failures.saturating_add(1) } else { 0 };
                                    }
                                }
                            }
                        }
                        if schedule.due && current.failures >= MAX_FAILURES { return Err(OrchError::NotAllowed("the previous executions failed repeatedly; review and resume this schedule".into())); }
                        current.admission_error = None; current.admission_retry_ms = None; current.admission_failures = 0;
                        if schedule.due { current.admission_slot_ms = None; }
                        task.schedule = Some(current.id.clone()); task.slot = Some(schedule.slot);
                        current.runs = current.runs.saturating_add(1); current.last_ms = Some(schedule.now); current.last_task = Some(task.id.clone());
                        if schedule.due { current.advance(schedule.slot, admitted_at); }
                        current.version = current.version.checked_add(1).ok_or_else(|| OrchError::NotAllowed("the schedule version limit was reached".into()))?;
                        if let Some(request) = schedule.request {
                            let key = crate::schedule::manual_receipt_id(project, &current.id, request)?;
                            tx.insert_record(crate::schedule::MANUAL_RECEIPT, &key, &serde_json::to_value(crate::ScheduleRunReceipt { schedule: current.id.clone(), request: request.into(), execution: task.id.clone(), kind: crate::ScheduleExecutionKind::Task, created_ms: now_ms() })?)?;
                            task.handoff_key = Some(key);
                        }
                        tx.put_record(SCHEDULE, &current.id, &serde_json::to_value(&current)?)?;
                    }
                    item.tasks.push(task.id.clone());
                    item.owner = Some(agent.name.clone());
                    item.state = WorkState::Doing;
                    item.done_ms = None;
                    item.blocked = None;
                    item.step(
                        &agent.name,
                        format!("started {}: {}", task.id, clip(&spec.objective, 100)),
                    );
                    tx.insert_record(TASK, &task.id, &serde_json::to_value(&task)?)?;
                    Some(task)
                } else {
                    item.state = WorkState::Doing;
                    item.done_ms = None;
                    item.blocked = None;
                    None
                };
                let last = match tx.get_record("orch_work_receipt_sequence", &item.id)? {
                    Some(value) => value["last"].as_u64().ok_or_else(|| {
                        OrchError::NotAllowed("the work receipt sequence is invalid".into())
                    })?,
                    None => 0,
                };
                let sequence = last.checked_add(1).ok_or_else(|| {
                    OrchError::NotAllowed("the work receipt sequence limit was reached".into())
                })?;
                tx.put_record(
                    "orch_work_receipt_sequence",
                    &item.id,
                    &serde_json::json!({"last":sequence}),
                )?;
                let receipt = WorkReceipt {
                    sequence,
                    parent: None,
                    id: id.clone(),
                    project: project.to_string(),
                    origin: origin.to_string(),
                    request: request.to_string(),
                    digest: digest.clone(),
                    conversation,
                    work: item.id.clone(),
                    task: task.as_ref().map(|t| t.id.clone()),
                    turn,
                    model: model.clone(),
                    created_ms: now_ms(),
                };
                let submission = Submission {
                    receipt,
                    target: target.clone(),
                    spec: spec.clone(),
                    artifacts,
                    rules,
                    agent: agent.as_ref().map(|agent| agent.spec.clone()),
                };
                tx.put_record(WORK, &item.id, &serde_json::to_value(&item)?)?;
                tx.insert_record(SUBMISSION, &id, &serde_json::to_value(&submission)?)?;
                Ok((Some(submission), true, Some(item), new_board, task))
            })?;
        let Some(submission) = submission else {
            return Ok(None);
        };
        if created {
            if let Some(board) = board {
                state.boards.insert(board.id.clone(), board.clone());
                self.emit(EventKind::Board {
                    board,
                    by: PERSON.to_string(),
                });
            }
            if let Some(item) = item {
                state.work.insert(item.id.clone(), item.clone());
                self.emit(EventKind::Work {
                    item,
                    by: PERSON.to_string(),
                });
            }
            if let Some(task) = task {
                let (done, _) = watch::channel(false);
                state.tasks.insert(
                    task.id.clone(),
                    TaskRt {
                        execution_lease: None,
                        execution_model: None,
                        record: task.clone(),
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
                self.emit(EventKind::TaskCreated {
                    task: task.id.clone(),
                    owner: task.owner,
                    origin: task.origin,
                    parent: None,
                    objective: clip(&task.objective, 300),
                    after: task.after,
                });
            }
        }
        Ok(Some(submission.receipt))
    }
}
