use super::*;
use ark_core::InvocationId;
use ark_runtime::{HarnessOutcome, Metrics};
use serde::{Deserialize, Serialize};

pub(super) const RESULT: &str = "orch_work_result";

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SavedResult {
    pub(super) conversation: String,
    pub(super) index: u32,
    pub(super) input: String,
    pub(super) answer: String,
    cited: Vec<InvocationId>,
    metrics: Metrics,
    steps: usize,
    trivial: bool,
    cancelled: bool,
    interrupted: bool,
    steered: Vec<String>,
    ran_out: bool,
}

impl SavedResult {
    pub(super) fn capture(session: &Session, input: &str, outcome: &HarnessOutcome) -> Self {
        Self {
            conversation: session.id().to_string(),
            index: session.turn_count(),
            input: input.into(),
            answer: outcome.answer.clone(),
            cited: outcome.cited.clone(),
            metrics: outcome.metrics.clone(),
            steps: outcome.steps,
            trivial: outcome.trivial,
            cancelled: outcome.cancelled,
            interrupted: outcome.interrupted,
            steered: outcome.steered.clone(),
            ran_out: outcome.ran_out,
        }
    }
    pub(super) fn outcome(&self) -> HarnessOutcome {
        HarnessOutcome {
            answer: self.answer.clone(),
            cited: self.cited.clone(),
            steps: self.steps,
            trivial: self.trivial,
            metrics: self.metrics.clone(),
            state: Default::default(),
            cancelled: self.cancelled,
            interrupted: self.interrupted,
            steered: self.steered.clone(),
            ran_out: self.ran_out,
        }
    }
}

impl Inner {
    fn publish_work_outputs(&self, id: &str) -> Result<(), OrchError> {
        let mut state = self.state.lock().unwrap();
        let outputs =
            self.engine
                .store
                .transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                    let mut task: TaskRecord = serde_json::from_value(
                        tx.get_record(TASK, id)?
                            .ok_or_else(|| OrchError::UnknownTask(id.into()))?,
                    )?;
                    let scope = match &task.execution_scope {
                        Some(scope) => scope.clone(),
                        None => {
                            let agent: AgentRecord = serde_json::from_value(
                                tx.get_record(
                                    AGENT,
                                    &format!("{}/{}", self.project.id, task.owner),
                                )?
                                .ok_or_else(|| OrchError::UnknownAgent(task.owner.clone()))?,
                            )?;
                            agent.scope
                        }
                    };
                    for invocation in tx.execution_invocations(&scope, id)? {
                        for id in invocation.produced {
                            let artifact =
                                tx.execution_artifact(&scope, id.as_str())?.ok_or_else(|| {
                                    OrchError::NotAllowed(
                                        "a produced work file is unavailable".into(),
                                    )
                                })?;
                            if artifact
                                .meta
                                .get("before")
                                .and_then(serde_json::Value::as_bool)
                                == Some(true)
                            {
                                continue;
                            }
                            if task.run.is_none() {
                                tx.put_artifact(&artifact)?;
                            }
                            if !task.outputs.iter().any(|file| file.id == id.as_str()) {
                                task.outputs.push(ArtifactRef {
                                    id: id.to_string(),
                                    name: artifact.name,
                                    kind: artifact.kind.as_tag(),
                                });
                            }
                        }
                    }
                    tx.put_record(TASK, id, &serde_json::to_value(&task)?)?;
                    Ok(task.outputs)
                })?;
        if let Some(task) = state.tasks.get_mut(id) {
            task.record.outputs = outputs;
        }
        Ok(())
    }

    pub(super) fn save_work_result(
        &self,
        id: &str,
        session: &mut Session,
        input: &str,
        outcome: &HarnessOutcome,
    ) -> Result<(), OrchError> {
        let result = SavedResult {
            conversation: session.id().to_string(),
            index: session.turn_count(),
            input: input.to_string(),
            answer: outcome.answer.clone(),
            cited: outcome.cited.clone(),
            metrics: outcome.metrics.clone(),
            steps: outcome.steps,
            trivial: outcome.trivial,
            cancelled: outcome.cancelled,
            interrupted: outcome.interrupted,
            steered: outcome.steered.clone(),
            ran_out: outcome.ran_out,
        };
        let value = serde_json::to_value(&result)?;
        self.engine
            .store
            .transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                let task: TaskRecord = serde_json::from_value(
                    tx.get_record(TASK, id)?
                        .ok_or_else(|| OrchError::UnknownTask(id.into()))?,
                )?;
                if task.conversation.as_deref() != Some(result.conversation.as_str())
                    || task.execution_owner.as_deref() != Some(&self.execution_token)
                {
                    return Err(OrchError::NotAllowed(
                        "this runtime does not own the work result".into(),
                    ));
                }
                if let Some(existing) = tx.get_record(RESULT, id)? {
                    if existing != value {
                        return Err(OrchError::NotAllowed(
                            "this task already has another saved result".into(),
                        ));
                    }
                } else {
                    tx.insert_record(RESULT, id, &value)?;
                }
                if let Some(run_id) = &task.run {
                    let mut run: crate::automation::Run = serde_json::from_value(
                        tx.get_record(crate::automation::RUN, run_id)?
                            .ok_or_else(|| {
                                OrchError::NotAllowed("the execution is unavailable".into())
                            })?,
                    )?;
                    if !outcome.answer.is_empty()
                        && run
                            .steps
                            .iter()
                            .any(|step| step.receipt.as_deref() == Some(id))
                    {
                        run.output.text = outcome.answer.clone();
                        tx.put_record(crate::automation::RUN, run_id, &serde_json::to_value(run)?)?;
                    }
                }
                Ok(())
            })?;
        self.publish_work_outputs(id)?;
        session.record_turn(&self.engine, input, outcome)?;
        Ok(())
    }

    pub(super) fn restore_work_result(&self, id: &str) -> Result<Option<Final>, OrchError> {
        let Some(value) = self.engine.store.get_record(RESULT, id)? else {
            return Ok(None);
        };
        let saved: SavedResult = serde_json::from_value(value)?;
        let restore = || -> Result<(), OrchError> {
            self.publish_work_outputs(id)?;
            let mut session = Session::open_for_execution(&self.engine, &saved.conversation)?
                .ok_or_else(|| OrchError::NotAllowed("the work conversation was deleted".into()))?;
            let scope = self.engine.store.transact_records::<_, OrchError>(
                self.project.id.as_str(),
                |tx| {
                    let task: TaskRecord = serde_json::from_value(
                        tx.get_record(TASK, id)?
                            .ok_or_else(|| OrchError::UnknownTask(id.into()))?,
                    )?;
                    tx.get_execution_session(&saved.conversation)?
                        .ok_or_else(|| {
                            OrchError::NotAllowed("the saved conversation is unavailable".into())
                        })?;
                    Ok(task
                        .execution_scope
                        .map(ProjectId)
                        .unwrap_or_else(|| self.project.id.clone()))
                },
            )?;
            if session.execution_project(&self.engine)? != scope {
                return Err(OrchError::NotAllowed(
                    "the work conversation belongs to another project".into(),
                ));
            }
            if session.turn_count() == saved.index {
                session.record_turn(&self.engine, &saved.input, &saved.outcome())?;
            } else {
                let turn = self
                    .engine
                    .store
                    .transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                        Ok(tx.get_execution_turn(&saved.conversation, saved.index)?)
                    })?
                    .ok_or_else(|| {
                        OrchError::NotAllowed("the saved work turn is missing".into())
                    })?;
                if session.turn_count() < saved.index
                    || turn["input"] != saved.input
                    || turn["answer"] != saved.answer
                {
                    return Err(OrchError::NotAllowed(
                        "the saved result conflicts with this conversation".into(),
                    ));
                }
            }
            Ok(())
        };
        Ok(Some(match restore() {
            Ok(()) if saved.cancelled => Final::cancelled("cancelled"),
            Ok(()) => Final::done(saved.answer),
            Err(error) => Final::saved_result_failed(
                saved.answer,
                format!("the answer is retained but its conversation could not be saved: {error}"),
            ),
        }))
    }

    pub(super) fn cleanup_work_result(&self, id: &str) -> Result<(), OrchError> {
        self.engine
            .store
            .transact_records(self.project.id.as_str(), |tx| {
                let Some(value) = tx.get_record(RESULT, id)? else {
                    return Ok::<_, OrchError>(());
                };
                let saved: SavedResult = serde_json::from_value(value)?;
                let task: TaskRecord = serde_json::from_value(
                    tx.get_record(TASK, id)?
                        .ok_or_else(|| OrchError::UnknownTask(id.into()))?,
                )?;
                if task.state == TaskState::Done
                    && task.result.as_deref() == Some(saved.answer.as_str())
                {
                    tx.delete_record(RESULT, id)?;
                }
                Ok(())
            })
    }

    pub(super) fn check_work_recovery(&self, id: &str) -> Result<(), OrchError> {
        let (task, scope) = {
            let state = self.state.lock().unwrap();
            let task = state
                .tasks
                .get(id)
                .ok_or_else(|| OrchError::UnknownTask(id.into()))?
                .record
                .clone();
            let scope = state
                .agents
                .get(&task.owner)
                .map(|agent| ProjectId(agent.record.scope.clone()))
                .ok_or_else(|| OrchError::UnknownAgent(task.owner.clone()))?;
            let scope = task
                .execution_scope
                .as_ref()
                .map(|scope| ProjectId(scope.clone()))
                .unwrap_or(scope);
            (task, scope)
        };
        if task.submission.is_none() || task.attempts == 0 {
            return Ok(());
        }
        let calls = self.engine.work_calls(&scope, id)?;
        if let Some(call) = calls.iter().find(|call| {
            call.risk == ark_core::RiskTier::Mutate
                && (call.status == ark_core::InvocationStatus::Running
                    || call.error.as_deref() == Some(ark_runtime::engine::INTERRUPTED_CALL))
        }) {
            return Err(OrchError::NotAllowed(format!(
                "{} may already have happened; check its result before continuing this work",
                call.action
            )));
        }
        Ok(())
    }
}
