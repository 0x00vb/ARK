use super::*;
use crate::automation::{Run, RunPhase, StepPhase};
use ark_core::SessionId;
use ark_runtime::session::{CarryPolicy, SessionRecord};
use ark_tools::{ActionOutput, Adapter, AdapterError, RunCtx};
use serde::{Deserialize, Serialize};

const CHILD: &str = "automation_child";

#[derive(Serialize, Deserialize)]
struct Child {
    run: String,
    step: String,
    parent: Option<String>,
    invocation: String,
    digest: String,
    receipt: String,
    task: String,
}

pub(super) fn descriptor(mut action: ark_core::ActionDescriptor) -> ark_core::ActionDescriptor {
    if action.name == tools::DELEGATE {
        action.input_schema["properties"]["wait"] = serde_json::json!({"type":"boolean","const":true,"default":true,"description":"wait for the delegated task's result"});
    } else if action.name == tools::WAIT {
        action.summary = "read results from tasks delegated by this execution".into();
    }
    action
}

pub(super) struct RunTools {
    pub inner: Arc<Inner>,
    pub run: Run,
    pub step: String,
    pub parent: Option<String>,
    pub capacity: Option<Arc<tokio::sync::Mutex<Slots>>>,
}

#[async_trait::async_trait]
impl Adapter for RunTools {
    fn name(&self) -> &str {
        "agents"
    }
    fn actions(&self) -> Vec<ark_core::ActionDescriptor> {
        AgentTools {
            orch: self.inner.me.clone(),
            agent: String::new(),
            task: String::new(),
            delegates: self
                .run
                .agents
                .iter()
                .map(|(name, spec)| (name.clone(), spec.role.clone()))
                .collect(),
            contacts: vec![],
            can_create: false,
            schedule: None,
        }
        .actions()
        .into_iter()
        .filter(|action| matches!(action.name.as_str(), tools::DELEGATE | tools::WAIT))
        .map(descriptor)
        .collect()
    }
    async fn run(
        &self,
        action: &str,
        input: serde_json::Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let fail = |error: OrchError| AdapterError::Tool(error.to_string());
        let o = Orchestrator {
            inner: self.inner.clone(),
        };
        let ids = match action {
            tools::DELEGATE => {
                if input.get("wait").is_some_and(|wait| wait != true) {
                    return Err(AdapterError::bad_input(
                        action,
                        "an automation waits for every delegated task",
                    ));
                }
                let invocation = ctx.invocation.as_deref().ok_or_else(|| {
                    AdapterError::bad_input(action, "delegation needs its running invocation")
                })?;
                let (receipt, task) = self
                    .inner
                    .admit_automation_child(
                        &self.run.id,
                        &self.step,
                        self.parent.as_deref(),
                        invocation,
                        &input,
                    )
                    .map_err(fail)?;
                o.execute_work(&receipt).map_err(fail)?;
                vec![task]
            }
            tools::WAIT => {
                let ids: Vec<String> = serde_json::from_value(input["tasks"].clone())
                    .map_err(|error| AdapterError::bad_input(action, error.to_string()))?;
                if ids.is_empty() || ids.len() > 32 {
                    return Err(AdapterError::bad_input(
                        action,
                        "choose one to 32 tasks from this step",
                    ));
                }
                self.inner
                    .engine
                    .store
                    .transact_records::<_, OrchError>(self.inner.project.id.as_str(), |tx| {
                        for id in &ids {
                            let child: Child = serde_json::from_value(
                                tx.get_record(CHILD, id)?.ok_or_else(|| {
                                    OrchError::NotAllowed(
                                        "that task was not delegated by this step".into(),
                                    )
                                })?,
                            )?;
                            if child.run != self.run.id
                                || child.step != self.step
                                || child.parent != self.parent
                            {
                                return Err(OrchError::NotAllowed(
                                    "that task belongs to another delegation".into(),
                                ));
                            }
                        }
                        Ok(())
                    })
                    .map_err(fail)?;
                ids
            }
            _ => return Err(AdapterError::UnknownAction(action.into())),
        };
        if let Some(capacity) = &self.capacity {
            let mut slots = capacity.lock().await;
            slots.waiters += 1;
            slots.account = None;
            slots.global = None;
        }
        let result = if let Some(parent) = &self.parent {
            Inner::wait_children(&self.inner, parent, &ids).await
        } else {
            async {
                let mut done = Vec::new();
                for id in &ids {
                    done.push(self.inner.wait_admitted(id).await?);
                }
                Ok(done)
            }
            .await
        };
        if let Some(capacity) = &self.capacity {
            let mut slots = capacity.lock().await;
            slots.waiters = slots.waiters.saturating_sub(1);
            if slots.waiters == 0 {
                self.inner
                    .run_guard(&self.run.id, true)
                    .and_then(|guard| guard.ensure_current())
                    .map_err(fail)?;
                let account = self.inner.account_slots(&slots.account_name);
                slots.account = Some(
                    account
                        .acquire_owned()
                        .await
                        .map_err(|error| AdapterError::Tool(error.to_string()))?,
                );
                slots.global = Some(
                    self.inner
                        .global
                        .clone()
                        .acquire_owned()
                        .await
                        .map_err(|error| AdapterError::Tool(error.to_string()))?,
                );
            }
        }
        let done = result.map_err(fail)?;
        let destination = self
            .inner
            .automation_parent_scope(&self.run.id, &self.step, self.parent.as_deref())
            .map_err(fail)?;
        let mut output = Vec::new();
        for task in done {
            if task.state != TaskState::Done {
                let reason = task
                    .error
                    .unwrap_or_else(|| "a delegated task did not complete".into());
                self.inner
                    .run_guard(&self.run.id, self.parent.is_none())
                    .map_err(fail)?
                    .fail(reason.clone());
                return Err(AdapterError::Tool(reason));
            }
            let source = ProjectId(task.execution_scope.clone().ok_or_else(|| {
                AdapterError::bad_input(action, "the child has no execution scope")
            })?);
            self.inner
                .share_artifacts(
                    &source,
                    &ProjectId(destination.clone()),
                    &task
                        .outputs
                        .iter()
                        .map(|file| file.id.clone())
                        .collect::<Vec<_>>(),
                )
                .map_err(fail)?;
            output.push(serde_json::json!({"task":task.id,"agent":task.owner,"state":task.state,"result":task.result,"error":task.error,"files":task.outputs}));
        }
        Ok(ActionOutput::json(serde_json::json!({"results":output})))
    }
}

impl Inner {
    pub(super) fn cancel_automation_tasks(&self, run: &str) {
        let ids: Vec<String> = self
            .state
            .lock()
            .unwrap()
            .tasks
            .values()
            .filter(|task| task.record.run.as_deref() == Some(run) && !task.record.state.is_final())
            .map(|task| task.record.id.clone())
            .collect();
        for id in ids {
            self.cancel_tree(&id, Stop::Cancel);
        }
    }

    pub(super) fn automation_parent_scope(
        &self,
        run: &str,
        step: &str,
        parent: Option<&str>,
    ) -> Result<String, OrchError> {
        self.engine
            .store
            .transact_records(self.project.id.as_str(), |tx| {
                let run: Run = serde_json::from_value(
                    tx.get_record(crate::automation::RUN, run)?.ok_or_else(|| {
                        OrchError::NotAllowed("the execution is unavailable".into())
                    })?,
                )?;
                let step = run
                    .steps
                    .iter()
                    .find(|candidate| candidate.id == step)
                    .ok_or_else(|| {
                        OrchError::NotAllowed("the execution step is unavailable".into())
                    })?;
                if let Some(parent) = parent {
                    let task: TaskRecord = serde_json::from_value(
                        tx.get_record(TASK, parent)?
                            .ok_or_else(|| OrchError::UnknownTask(parent.into()))?,
                    )?;
                    if task.run.as_deref() != Some(&run.id)
                        || task.execution_step.as_deref() != Some(&step.id)
                    {
                        return Err(OrchError::NotAllowed(
                            "the parent belongs to another execution step".into(),
                        ));
                    }
                    task.execution_scope.ok_or_else(|| {
                        OrchError::NotAllowed("the parent has no execution scope".into())
                    })
                } else {
                    Ok(step.scope.clone())
                }
            })
    }

    fn admit_automation_child(
        &self,
        run_id: &str,
        step_id: &str,
        parent: Option<&str>,
        invocation: &str,
        input: &serde_json::Value,
    ) -> Result<(String, String), OrchError> {
        let scope = self.automation_parent_scope(run_id, step_id, parent)?;
        let guard = self.run_guard(run_id, parent.is_none())?;
        guard.ensure_current()?;
        let digest = blake3::hash(&serde_json::to_vec(&(run_id, step_id, parent, input))?)
            .to_hex()
            .to_string();
        let task_id = format!(
            "t-{}",
            blake3::hash(format!("{run_id}/{invocation}").as_bytes()).to_hex()
        );
        let mut state = self.state.lock().unwrap();
        let (child, changed_parent) =
            self.engine
                .store
                .transact_records(self.project.id.as_str(), |tx| {
                    let run = guard.verify_child_call(tx, &scope, invocation, input)?;
                    let step = run
                        .steps
                        .iter()
                        .find(|step| step.id == step_id && step.phase == StepPhase::Running)
                        .ok_or_else(|| {
                            OrchError::NotAllowed("delegation is outside the current step".into())
                        })?;
                    if !matches!(run.phase, RunPhase::Running | RunPhase::WaitingApproval) {
                        return Err(OrchError::NotAllowed(
                            "this execution is not running".into(),
                        ));
                    }
                    if let Some(value) = tx.get_record(CHILD, &task_id)? {
                        let child: Child = serde_json::from_value(value)?;
                        if child.digest != digest {
                            return Err(OrchError::NotAllowed(
                                "this delegation identity names another brief".into(),
                            ));
                        }
                        return Ok::<_, OrchError>((child, None));
                    }
                    let to = input["to"]
                        .as_str()
                        .ok_or_else(|| OrchError::NotAllowed("choose an approved agent".into()))?;
                    let frozen = run.agents.get(to).ok_or_else(|| {
                        OrchError::NotAllowed("the delegation target was not approved".into())
                    })?;
                    let current: AgentRecord = serde_json::from_value(
                        tx.get_record(AGENT, &format!("{}/{to}", self.project.id))?
                            .ok_or_else(|| OrchError::UnknownAgent(to.into()))?,
                    )?;
                    if matches!(
                        current.state,
                        AgentState::Suspended | AgentState::Terminated
                    ) {
                        return Err(OrchError::NotAllowed(
                            "the selected agent is unavailable".into(),
                        ));
                    }
                    let mut parent_record = parent
                        .map(|id| -> Result<TaskRecord, OrchError> {
                            Ok(serde_json::from_value(
                                tx.get_record(TASK, id)?
                                    .ok_or_else(|| OrchError::UnknownTask(id.into()))?,
                            )?)
                        })
                        .transpose()?;
                    if let Some(record) = &parent_record {
                        if record.execution_scope.as_deref() != Some(scope.as_str())
                            || record.run.as_deref() != Some(run_id)
                            || record.execution_step.as_deref() != Some(step_id)
                        {
                            return Err(OrchError::NotAllowed(
                                "the delegation parent changed".into(),
                            ));
                        }
                        let from = run.agents.get(&record.owner).ok_or_else(|| {
                            OrchError::NotAllowed("the parent agent was not approved".into())
                        })?;
                        if !from.may_delegate(to)
                            || record.state.is_final()
                            || record.execution_owner.as_deref() != Some(&self.execution_token)
                        {
                            return Err(OrchError::NotAllowed(
                                "this parent cannot delegate to that agent".into(),
                            ));
                        }
                        if record
                            .budget
                            .max_delegations
                            .is_some_and(|max| record.children.len() >= max as usize)
                        {
                            return Err(OrchError::NotAllowed(
                                "the parent's delegation budget was reached".into(),
                            ));
                        }
                        let mut ancestor = Some(record.clone());
                        let mut depth = 0;
                        while let Some(record) = ancestor {
                            depth += 1;
                            if depth >= 8
                                || record.owner == to
                                || record.run.as_deref() != Some(run_id)
                                || record.execution_step.as_deref() != Some(step_id)
                            {
                                return Err(OrchError::NotAllowed(
                                    "delegation would repeat an ancestor or exceed eight levels"
                                        .into(),
                                ));
                            }
                            ancestor = record
                                .parent
                                .as_deref()
                                .map(|id| -> Result<TaskRecord, OrchError> {
                                    Ok(serde_json::from_value(
                                        tx.get_record(TASK, id)?
                                            .ok_or_else(|| OrchError::UnknownTask(id.into()))?,
                                    )?)
                                })
                                .transpose()?;
                        }
                    }
                    let objective = input["objective"]
                        .as_str()
                        .filter(|value| !value.trim().is_empty() && value.len() <= 8000)
                        .ok_or_else(|| {
                            OrchError::NotAllowed(
                                "a delegation needs a brief of at most 8000 bytes".into(),
                            )
                        })?;
                    let context = input["context"].as_str().unwrap_or("");
                    if context.len() > 16000 {
                        return Err(OrchError::NotAllowed(
                            "the delegation context is too long".into(),
                        ));
                    }
                    let ids: Vec<String> = input
                        .get("artifacts")
                        .map(|value| serde_json::from_value(value.clone()))
                        .transpose()?
                        .unwrap_or_default();
                    if ids.len() > 32 {
                        return Err(OrchError::NotAllowed(
                            "delegate at most 32 explicit files".into(),
                        ));
                    }
                    let mut artifacts = Vec::new();
                    for id in ids {
                        artifacts.push(
                            tx.execution_artifact(&scope, id.trim_start_matches("art:"))?
                                .ok_or_else(|| {
                                    OrchError::NotAllowed(
                                        "an explicitly delegated file is unavailable".into(),
                                    )
                                })?,
                        );
                    }
                    let mut spec = TaskSpec::new(objective).context(context);
                    spec.artifacts = artifacts
                        .iter()
                        .map(|artifact| artifact.id.to_string())
                        .collect();
                    spec.item = Some(run.work.clone());
                    spec.max_attempts = Some(1);
                    let child_scope =
                        Project::new(objective, "").with_parent(self.project.id.clone());
                    tx.create_execution_project(&child_scope)?;
                    let mut session = SessionRecord::empty(
                        SessionId::new(),
                        child_scope.id.clone(),
                        CarryPolicy::default(),
                        frozen.model.clone(),
                    );
                    session.work_item = Some(run.work.clone());
                    session.execution_managed = true;
                    session.title = clip(objective, 120);
                    tx.create_execution_session(
                        session.id.as_str(),
                        &serde_json::to_value(&session)?,
                    )?;
                    let receipt_id = format!("wr-{}", blake3::hash(task_id.as_bytes()).to_hex());
                    let mut task = TaskRecord::queued(
                        task_id.clone(),
                        run.project.clone(),
                        to.into(),
                        &spec,
                        Origin::External,
                        frozen.budget.clone(),
                        artifacts
                            .iter()
                            .map(|artifact| ArtifactRef {
                                id: artifact.id.to_string(),
                                name: artifact.name.clone(),
                                kind: artifact.kind.as_tag(),
                            })
                            .collect(),
                    );
                    task.parent = parent.map(str::to_string);
                    task.run = Some(run.id.clone());
                    task.execution_step = Some(step.id.clone());
                    task.execution_scope = Some(child_scope.id.to_string());
                    task.conversation = Some(session.id.to_string());
                    task.submission = Some(receipt_id.clone());
                    task.item = Some(run.work.clone());
                    let submission = admission::Submission {
                        receipt: WorkReceipt {
                            id: receipt_id.clone(),
                            project: run.project.clone(),
                            origin: "automation-child".into(),
                            request: invocation.into(),
                            digest: digest.clone(),
                            conversation: session.id.to_string(),
                            work: run.work.clone(),
                            task: Some(task_id.clone()),
                            turn: None,
                            model: frozen.model.clone().ok_or_else(|| {
                                OrchError::NotAllowed("the child's frozen model is missing".into())
                            })?,
                            created_ms: now_ms(),
                            sequence: 0,
                            parent: Some(WorkRunRef::Automation {
                                run: run.id.clone(),
                            }),
                        },
                        target: WorkTarget::Agent { name: to.into() },
                        spec,
                        artifacts,
                        rules: vec![],
                        agent: Some(frozen.clone()),
                        images: Vec::new(),
                    };
                    let child = Child {
                        run: run_id.into(),
                        step: step_id.into(),
                        parent: parent.map(str::to_string),
                        invocation: invocation.into(),
                        digest: digest.clone(),
                        receipt: receipt_id.clone(),
                        task: task_id.clone(),
                    };
                    tx.insert_record(TASK, &task_id, &serde_json::to_value(task)?)?;
                    tx.insert_record(
                        admission::SUBMISSION,
                        &receipt_id,
                        &serde_json::to_value(submission)?,
                    )?;
                    tx.insert_record(CHILD, &task_id, &serde_json::to_value(&child)?)?;
                    if let Some(record) = &mut parent_record {
                        record.children.push(task_id.clone());
                        tx.put_record(TASK, &record.id, &serde_json::to_value(&record)?)?;
                    }
                    Ok((child, parent_record))
                })?;
        if let Some(record) = changed_parent {
            if let Some(current) = state.tasks.get_mut(&record.id) {
                current.record = record;
            }
        }
        Ok((child.receipt, child.task))
    }
}
