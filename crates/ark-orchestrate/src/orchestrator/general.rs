use super::*;
use serde::{Deserialize, Serialize};

pub(super) const GENERAL_SLOT: &str = "orch_work_general_turn";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct GeneralSlot {
    pub receipt: String,
    pub owner: Option<GeneralOwner>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct GeneralOwner {
    token: String,
    pid: u32,
    pid_start: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneralWorkState {
    Pending,
    Starting,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Deleted,
}

#[derive(Clone, Debug)]
pub enum GeneralWorkStart {
    Ready {
        receipt: WorkReceipt,
        input: String,
    },
    Observed {
        receipt: WorkReceipt,
        state: GeneralWorkState,
    },
}

pub(super) fn state(
    tx: &ark_store::RecordTransaction<'_>,
    receipt: &WorkReceipt,
) -> Result<GeneralWorkState, OrchError> {
    if let Some(value) = tx.get_record(GENERAL_RESULT, &receipt.id)? {
        let saved: GeneralWorkResult = serde_json::from_value(value)?;
        if saved.state == GeneralWorkState::Cancelled {
            return Ok(saved.state);
        }
    }
    let Some(header) = tx.get_session(&receipt.conversation)? else {
        return Ok(GeneralWorkState::Deleted);
    };
    let index = receipt
        .turn
        .ok_or_else(|| OrchError::NotAllowed("this is not a General turn".into()))?;
    let slot_id = format!("{}/{}", receipt.conversation, index);
    let slot: GeneralSlot =
        serde_json::from_value(tx.get_record(GENERAL_SLOT, &slot_id)?.ok_or_else(|| {
            OrchError::NotAllowed("the admitted General turn is missing".into())
        })?)?;
    let owner_alive = slot
        .owner
        .as_ref()
        .is_some_and(|owner| process::writer_alive(owner.pid, owner.pid_start));
    if let Some(turn) = tx.get_session_turn(&receipt.conversation, index)? {
        if header["turn_count"]
            .as_u64()
            .is_some_and(|count| count > u64::from(index))
        {
            if turn["error"].as_str() == Some(ark_runtime::session::INTERRUPTED) {
                return Ok(GeneralWorkState::Interrupted);
            }
            return Ok(if turn["cancelled"] == true {
                GeneralWorkState::Cancelled
            } else if turn["incomplete"] == true || !turn["error"].is_null() {
                GeneralWorkState::Failed
            } else {
                GeneralWorkState::Completed
            });
        }
        let alive = turn["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .is_some_and(|pid| process::writer_alive(pid, turn["pid_start"].as_u64()));
        return Ok(if alive && owner_alive {
            GeneralWorkState::Running
        } else {
            GeneralWorkState::Interrupted
        });
    }
    if header["turn_count"].as_u64() != Some(u64::from(index)) {
        return Ok(GeneralWorkState::Interrupted);
    }
    Ok(if owner_alive {
        GeneralWorkState::Starting
    } else {
        GeneralWorkState::Pending
    })
}

fn submission_input(submission: &admission::Submission) -> String {
    let mut input = submission.spec.objective.clone();
    if !submission.spec.context.trim().is_empty() {
        input.push_str("\n\n");
        input.push_str(&submission.spec.context);
    }
    if !submission.artifacts.is_empty() {
        input.push_str("\n\n== INPUTS ==");
        for artifact in &submission.artifacts {
            input.push_str(&format!(
                "\n- art:{} {} ({})",
                artifact.id.0,
                artifact.name,
                artifact.kind.as_tag()
            ));
        }
    }
    input
}

impl Orchestrator {
    /// Reserve the initial General turn before scheduling the existing conversation loop.
    /// A stored begun turn is observed, never replayed by this operation.
    pub fn cancel_unstarted_general_work(&self, id: &str) -> Result<bool, OrchError> {
        self.inner.with_work(|data, tx| {
            let submission: admission::Submission =
                serde_json::from_value(tx.get_record(admission::SUBMISSION, id)?.ok_or_else(
                    || OrchError::NotAllowed("the admitted message is unavailable".into()),
                )?)?;
            if !matches!(
                state(tx, &submission.receipt)?,
                GeneralWorkState::Pending | GeneralWorkState::Starting
            ) {
                return Ok((false, vec![]));
            }
            let index = submission
                .receipt
                .turn
                .ok_or_else(|| OrchError::NotAllowed("this work uses an agent".into()))?;
            if tx
                .get_session_turn(&submission.receipt.conversation, index)?
                .is_some()
            {
                return Ok((false, vec![]));
            }
            let result = GeneralWorkResult {
                receipt: id.to_string(),
                conversation: submission.receipt.conversation.clone(),
                turn: index,
                state: GeneralWorkState::Cancelled,
                answer: None,
                error: None,
                outputs: vec![],
                at_ms: Some(now_ms()),
            };
            tx.insert_record(GENERAL_RESULT, id, &serde_json::to_value(result)?)?;
            let slot_id = format!("{}/{}", submission.receipt.conversation, index);
            let slot: GeneralSlot =
                serde_json::from_value(tx.get_record(GENERAL_SLOT, &slot_id)?.ok_or_else(
                    || OrchError::NotAllowed("the admitted General turn is missing".into()),
                )?)?;
            if slot.owner.is_none() {
                tx.delete_record(GENERAL_SLOT, &slot_id)?;
            }
            let item = data
                .items
                .get_mut(&submission.receipt.work)
                .ok_or_else(|| OrchError::NotAllowed("the work item is unavailable".into()))?;
            item.step(PERSON, "cancelled before starting");
            tx.put_record(WORK, &item.id, &serde_json::to_value(&*item)?)?;
            Ok((
                true,
                vec![EventKind::Work {
                    item: item.clone(),
                    by: PERSON.into(),
                }],
            ))
        })
    }

    pub async fn general_work_models(
        &self,
        id: &str,
    ) -> Result<crate::models::AgentModels, OrchError> {
        let receipt = self
            .work_receipt_by_id(id)?
            .ok_or_else(|| OrchError::NotAllowed("the admitted message is unavailable".into()))?;
        if receipt.turn.is_none() {
            return Err(OrchError::NotAllowed("this work uses an agent".into()));
        }
        let submission: super::admission::Submission = serde_json::from_value(
            self.inner
                .engine
                .store
                .get_record(super::admission::SUBMISSION, id)?
                .ok_or_else(|| {
                    OrchError::NotAllowed("the admitted message is unavailable".into())
                })?,
        )?;
        let session = Session::open(&self.inner.engine, &receipt.conversation)?
            .ok_or_else(|| OrchError::NotAllowed("the work conversation was deleted".into()))?;
        if session.record().work_item.as_deref() != Some(&receipt.work) {
            return Err(OrchError::NotAllowed(
                "the admitted message belongs to another conversation".into(),
            ));
        }
        let scope = session.execution_project(&self.inner.engine)?;
        for artifact in &submission.artifacts {
            self.inner.engine.store.put_artifact(&scope, artifact)?;
        }
        let lane = format!(
            "{}/work/general/{}",
            self.project().id,
            receipt.conversation
        );
        let models = self
            .inner
            .models
            .resolve(&lane, Some(&receipt.model))
            .await?;
        if self.inner.models.selection_spec(&models) != receipt.model {
            self.inner.models.close(&lane).await;
            return Err(OrchError::NotAllowed(
                "the admitted model selection is unavailable".into(),
            ));
        }
        Ok(models)
    }

    pub async fn close_general_work_models(&self, conversation: &str) {
        self.inner
            .models
            .close(&format!(
                "{}/work/general/{conversation}",
                self.project().id
            ))
            .await;
    }

    pub fn general_work_input(&self, id: &str) -> Result<String, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let value = tx.get_record(admission::SUBMISSION, id)?.ok_or_else(|| {
                    OrchError::NotAllowed("the admitted message is unavailable".into())
                })?;
                let submission: admission::Submission = serde_json::from_value(value)?;
                if submission.receipt.turn.is_none() {
                    return Err(OrchError::NotAllowed("this work uses an agent".into()));
                }
                Ok(submission_input(&submission))
            })
    }

    pub fn begin_general_work(&self, id: &str) -> Result<GeneralWorkStart, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let value = tx.get_record(admission::SUBMISSION, id)?.ok_or_else(|| {
                    OrchError::NotAllowed("no work submission with that identity".into())
                })?;
                let submission: admission::Submission = serde_json::from_value(value)?;
                if !matches!(submission.target, WorkTarget::General { .. }) {
                    return Err(OrchError::NotAllowed("this work runs with an agent".into()));
                }
                let observed = state(tx, &submission.receipt)?;
                if observed != GeneralWorkState::Pending {
                    return Ok(GeneralWorkStart::Observed {
                        receipt: submission.receipt,
                        state: observed,
                    });
                }
                let index = submission
                    .receipt
                    .turn
                    .ok_or_else(|| OrchError::NotAllowed("the General turn has no index".into()))?;
                let slot_id = format!("{}/{}", submission.receipt.conversation, index);
                let mut slot: GeneralSlot =
                    serde_json::from_value(tx.get_record(GENERAL_SLOT, &slot_id)?.ok_or_else(
                        || OrchError::NotAllowed("the admitted General turn is missing".into()),
                    )?)?;
                if slot.receipt != id {
                    return Err(OrchError::NotAllowed(
                        "the General turn belongs to another submission".into(),
                    ));
                }
                slot.owner = Some(GeneralOwner {
                    token: self.inner.execution_token.clone(),
                    pid: std::process::id(),
                    pid_start: process::own_start(),
                });
                tx.put_record(GENERAL_SLOT, &slot_id, &serde_json::to_value(&slot)?)?;
                let input = submission_input(&submission);
                Ok(GeneralWorkStart::Ready {
                    receipt: submission.receipt,
                    input,
                })
            })
    }

    pub fn release_unstarted_general_work(&self, id: &str) -> Result<(), OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let submission: admission::Submission =
                    serde_json::from_value(tx.get_record(admission::SUBMISSION, id)?.ok_or_else(
                        || OrchError::NotAllowed("no work submission with that identity".into()),
                    )?)?;
                let index = submission
                    .receipt
                    .turn
                    .ok_or_else(|| OrchError::NotAllowed("this is not a General turn".into()))?;
                if tx
                    .get_session_turn(&submission.receipt.conversation, index)?
                    .is_some()
                {
                    return Err(OrchError::NotAllowed(
                        "this General turn already began".into(),
                    ));
                }
                let slot_id = format!("{}/{}", submission.receipt.conversation, index);
                let mut slot: GeneralSlot =
                    serde_json::from_value(tx.get_record(GENERAL_SLOT, &slot_id)?.ok_or_else(
                        || OrchError::NotAllowed("the General turn is missing".into()),
                    )?)?;
                if slot.receipt != id
                    || slot
                        .owner
                        .as_ref()
                        .is_some_and(|owner| owner.token != self.inner.execution_token)
                {
                    return Err(OrchError::NotAllowed(
                        "this runtime does not own the unstarted turn".into(),
                    ));
                }
                slot.owner = None;
                tx.put_record(GENERAL_SLOT, &slot_id, &serde_json::to_value(&slot)?)?;
                Ok(())
            })
    }

    pub fn general_work_state(&self, id: &str) -> Result<GeneralWorkState, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let submission: admission::Submission =
                    serde_json::from_value(tx.get_record(admission::SUBMISSION, id)?.ok_or_else(
                        || OrchError::NotAllowed("no work submission with that identity".into()),
                    )?)?;
                state(tx, &submission.receipt)
            })
    }
}

pub(super) const GENERAL_RESULT: &str = "orch_work_general_result";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneralWorkResult {
    pub receipt: String,
    pub conversation: String,
    pub turn: u32,
    pub state: GeneralWorkState,
    pub answer: Option<String>,
    pub error: Option<String>,
    pub outputs: Vec<ArtifactRef>,
    pub at_ms: Option<u64>,
}

pub(super) fn result(
    tx: &ark_store::RecordTransaction<'_>,
    receipt: &WorkReceipt,
) -> Result<Option<GeneralWorkResult>, OrchError> {
    if let Some(value) = tx.get_record(GENERAL_RESULT, &receipt.id)? {
        return Ok(Some(serde_json::from_value(value)?));
    }
    let phase = state(tx, receipt)?;
    if !matches!(
        phase,
        GeneralWorkState::Completed
            | GeneralWorkState::Failed
            | GeneralWorkState::Cancelled
            | GeneralWorkState::Interrupted
    ) {
        return Ok(None);
    }
    let index = receipt
        .turn
        .ok_or_else(|| OrchError::NotAllowed("this is not a General turn".into()))?;
    let Some(header) = tx.get_session(&receipt.conversation)? else {
        return Ok(None);
    };
    if !header["turn_count"]
        .as_u64()
        .is_some_and(|count| count > u64::from(index))
    {
        return Ok(None);
    }
    let Some(value) = tx.get_session_turn(&receipt.conversation, index)? else {
        return Ok(None);
    };
    let turn: ark_runtime::session::Turn = serde_json::from_value(value)?;
    let mut outputs = Vec::new();
    let mut file_error = None;
    let scope = header["execution_scope"]
        .as_str()
        .unwrap_or(&receipt.project);
    tx.validate_execution_project(scope)?;
    if scope != receipt.project {
        let binding = tx
            .get_record("session_execution_scope", scope)?
            .ok_or_else(|| {
                OrchError::NotAllowed("the conversation's work context is unavailable".into())
            })?;
        if binding["session"] != receipt.conversation || binding["work"] != receipt.work {
            return Err(OrchError::NotAllowed(
                "the conversation's work context belongs to another work item".into(),
            ));
        }
    }
    let work = format!("session:{}/{index}", receipt.conversation);
    for invocation in tx.execution_invocations(scope, &work)? {
        for id in invocation.produced {
            if outputs
                .iter()
                .any(|artifact: &ArtifactRef| artifact.id == id.as_str())
            {
                continue;
            }
            let artifact = tx.execution_artifact(scope, id.as_str())?;
            if let Some(artifact) = artifact {
                if artifact
                    .meta
                    .get("before")
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
                {
                    outputs.push(ArtifactRef {
                        id: artifact.id.to_string(),
                        name: artifact.name,
                        kind: artifact.kind.as_tag(),
                    });
                }
            } else {
                file_error = Some(
                    "A file produced by this turn is no longer available. The answer is retained."
                        .to_string(),
                );
            }
        }
    }
    Ok(Some(GeneralWorkResult {
        receipt: receipt.id.clone(),
        conversation: receipt.conversation.clone(),
        turn: index,
        state: if file_error.is_some() {
            GeneralWorkState::Failed
        } else {
            phase
        },
        answer: (!turn.answer.is_empty()).then_some(turn.answer),
        error: turn.error.or(file_error),
        outputs,
        at_ms: u64::try_from(turn.at.unix_timestamp_nanos() / 1_000_000).ok(),
    }))
}

impl Orchestrator {
    pub fn settle_general_work(&self, id: &str) -> Result<Option<GeneralWorkResult>, OrchError> {
        self.inner.with_work(|data, tx| {
            let submission: admission::Submission =
                serde_json::from_value(tx.get_record(admission::SUBMISSION, id)?.ok_or_else(
                    || OrchError::NotAllowed("no work submission with that identity".into()),
                )?)?;
            if tx.get_record(GENERAL_RESULT, id)?.is_some() {
                return Ok((result(tx, &submission.receipt)?, vec![]));
            }
            let Some(result) = result(tx, &submission.receipt)? else {
                return Ok((None, vec![]));
            };
            tx.insert_record(GENERAL_RESULT, id, &serde_json::to_value(&result)?)?;
            let mut events = Vec::new();
            if let Some(item) = data.items.get_mut(&submission.receipt.work) {
                let open = item
                    .tasks
                    .iter()
                    .filter_map(|id| data.tasks.get(id))
                    .any(|task| !task.state.is_final());
                if !open {
                    match result.state {
                        GeneralWorkState::Completed => {
                            item.state = WorkState::Done;
                            item.done_ms = result.at_ms;
                            item.blocked = None;
                            item.step(
                                PERSON,
                                format!(
                                    "finished: {}",
                                    clip(result.answer.as_deref().unwrap_or(""), 200)
                                ),
                            );
                        }
                        GeneralWorkState::Failed | GeneralWorkState::Interrupted => {
                            let error = result
                                .error
                                .clone()
                                .unwrap_or_else(|| "the conversation was interrupted".into());
                            item.blocked = Some(error.clone());
                            item.step(PERSON, clip(&error, 200));
                        }
                        GeneralWorkState::Cancelled => {
                            item.state = WorkState::Todo;
                            item.blocked = None;
                            item.step(PERSON, "cancelled");
                        }
                        _ => {}
                    }
                    tx.put_record(WORK, &item.id, &serde_json::to_value(&item)?)?;
                    events.push(EventKind::Work {
                        item: item.clone(),
                        by: PERSON.to_string(),
                    });
                }
            }
            Ok((Some(result), events))
        })
    }
}

impl Orchestrator {
    pub fn validate_general_input(&self, conversation: &str, input: &str) -> Result<(), OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let header: ark_runtime::session::SessionRecord =
                    serde_json::from_value(tx.get_session(conversation)?.ok_or_else(|| {
                        OrchError::NotAllowed("the conversation was deleted".into())
                    })?)?;
                if header.work_item.is_none() {
                    return Ok(());
                }
                if let Some(work) = &header.work_item {
                    let item: WorkItem =
                        serde_json::from_value(tx.get_record(WORK, work)?.ok_or_else(|| {
                            OrchError::NotAllowed("the work item is unavailable".into())
                        })?)?;
                    if let Some(id) = item.run {
                        let run: crate::automation::Run = serde_json::from_value(
                            tx.get_record(crate::automation::RUN, &id)?.ok_or_else(|| {
                                OrchError::NotAllowed("the automation execution is missing".into())
                            })?,
                        )?;
                        if !run.phase.is_final() {
                            return Err(OrchError::NotAllowed(
                                "wait for this automation to finish before adding a message".into(),
                            ));
                        }
                    }
                }
                if let Some(value) = tx.get_record(
                    GENERAL_SLOT,
                    &format!("{conversation}/{}", header.turn_count),
                )? {
                    let slot: GeneralSlot = serde_json::from_value(value)?;
                    let submission: admission::Submission = serde_json::from_value(
                        tx.get_record(admission::SUBMISSION, &slot.receipt)?
                            .ok_or_else(|| {
                                OrchError::NotAllowed("the admitted message is missing".into())
                            })?,
                    )?;
                    let expected = submission_input(&submission);
                    if expected != input {
                        return Err(OrchError::NotAllowed(
                            "this conversation has a different admitted message waiting to start"
                                .into(),
                        ));
                    }
                    if slot
                        .owner
                        .is_some_and(|owner| process::writer_alive(owner.pid, owner.pid_start))
                    {
                        return Err(OrchError::NotAllowed(
                            "this message is already starting; reopen the conversation".into(),
                        ));
                    }
                }
                Ok(())
            })
    }

    pub async fn prepare_general_turn(
        &self,
        conversation: &str,
        input: &str,
        model: Option<String>,
    ) -> Result<WorkReceipt, OrchError> {
        let (work, pending, target) =
            self.inner
                .engine
                .store
                .transact_records(self.project().id.as_str(), |tx| {
                    let header: ark_runtime::session::SessionRecord =
                        serde_json::from_value(tx.get_session(conversation)?.ok_or_else(
                            || OrchError::NotAllowed("the conversation was deleted".into()),
                        )?)?;
                    let work = header.work_item.ok_or_else(|| {
                        OrchError::NotAllowed("this conversation has no work binding".into())
                    })?;
                    let pending = tx
                        .get_record(
                            GENERAL_SLOT,
                            &format!("{conversation}/{}", header.turn_count),
                        )?
                        .map(serde_json::from_value::<GeneralSlot>)
                        .transpose()?
                        .map(|slot| slot.receipt);
                    let mut latest = None;
                    for value in tx.list_records(admission::SUBMISSION)? {
                        let submission: admission::Submission = serde_json::from_value(value)?;
                        if submission.receipt.work == work
                            && submission.receipt.parent.is_none()
                            && latest.as_ref().is_none_or(|(sequence, _)| {
                                submission.receipt.sequence >= *sequence
                            })
                        {
                            latest = Some((submission.receipt.sequence, submission.target));
                        }
                    }
                    Ok::<_, OrchError>((work, pending, latest.map(|(_, target)| target)))
                })?;
        self.validate_general_input(conversation, input)?;
        let receipt = match pending {
            Some(id) => self
                .work_receipt_by_id(&id)?
                .ok_or_else(|| OrchError::NotAllowed("the admitted message is missing".into()))?,
            None => {
                if matches!(target, Some(WorkTarget::Agent { .. })) {
                    return Err(OrchError::NotAllowed(
                        "this conversation belongs to an agent; send the message through Work"
                            .into(),
                    ));
                }
                self.admit_work(
                    WorkTarget::General { model },
                    TaskSpec::new(input).item(work),
                    "conversation",
                    &uuid::Uuid::new_v4().to_string(),
                )
                .await?
            }
        };
        match self.begin_general_work(&receipt.id)? {
            GeneralWorkStart::Ready { .. } => Ok(receipt),
            GeneralWorkStart::Observed { .. } => Err(OrchError::NotAllowed(
                "this message already started; reopen the conversation to read its result".into(),
            )),
        }
    }

    pub fn end_general_work(&self, id: &str) -> Result<(), OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let submission: admission::Submission =
                    serde_json::from_value(tx.get_record(admission::SUBMISSION, id)?.ok_or_else(
                        || OrchError::NotAllowed("the admitted message is missing".into()),
                    )?)?;
                let index = submission
                    .receipt
                    .turn
                    .ok_or_else(|| OrchError::NotAllowed("this is not a General turn".into()))?;
                let key = format!("{}/{}", submission.receipt.conversation, index);
                let cancelled = state(tx, &submission.receipt)? == GeneralWorkState::Cancelled;
                let Some(value) = tx.get_record(GENERAL_SLOT, &key)? else {
                    if cancelled {
                        return Ok(());
                    }
                    return Err(OrchError::NotAllowed(
                        "the admitted General turn is missing".into(),
                    ));
                };
                let mut slot: GeneralSlot = serde_json::from_value(value)?;
                if slot.receipt != id {
                    if cancelled {
                        return Ok(());
                    }
                    return Err(OrchError::NotAllowed(
                        "the General turn belongs to another submission".into(),
                    ));
                }
                if slot
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.token != self.inner.execution_token)
                {
                    return Err(OrchError::NotAllowed(
                        "another runtime owns this General turn".into(),
                    ));
                }
                if cancelled
                    && tx
                        .get_session_turn(&submission.receipt.conversation, index)?
                        .is_none()
                {
                    tx.delete_record(GENERAL_SLOT, &key)?;
                    return Ok(());
                }
                slot.owner = None;
                tx.put_record(GENERAL_SLOT, &key, &serde_json::to_value(&slot)?)?;
                Ok(())
            })
    }
}
