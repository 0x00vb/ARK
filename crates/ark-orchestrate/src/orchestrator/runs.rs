use super::*;
use crate::automation::{
    self, ActivationPhase, InputValue, Output, Run, RunOrigin, RunOwner, RunPhase, StepExecution,
    StepPhase,
};
use ark_core::SessionId;
use ark_runtime::session::{CarryPolicy, SessionRecord};
use std::collections::BTreeMap;

const TURN_RESULT: &str = "automation_turn_result";

struct GeneralTurn<'a> {
    run: &'a Run,
    index: usize,
    selected: &'a automation::FrozenSelection,
    session: &'a mut Session,
    message: &'a str,
}

pub(super) struct FrozenRunAdmission<'a> {
    pub automation: &'a str,
    pub expected: u64,
    pub origin: RunOrigin,
    pub inputs: BTreeMap<String, InputValue>,
    pub input_digest: String,
    pub prepared: automations::PreparedActivation,
    pub due_now: Option<u64>,
    pub authenticated_connection: Option<(&'a str, u64)>,
}

fn unavailable() -> OrchError {
    OrchError::NotAllowed("the automation execution is unavailable".into())
}
fn load(tx: &ark_store::RecordTransaction<'_>, id: &str) -> Result<Run, OrchError> {
    Ok(serde_json::from_value(
        tx.get_record(automation::RUN, id)?
            .ok_or_else(unavailable)?,
    )?)
}
fn save(tx: &ark_store::RecordTransaction<'_>, run: &mut Run) -> Result<(), OrchError> {
    run.updated_ms = now_ms();
    tx.put_record(automation::RUN, &run.id, &serde_json::to_value(&*run)?)?;
    Ok(())
}
fn owned(run: &Run, token: &str) -> Result<(), OrchError> {
    if run.owner.as_ref().is_none_or(|owner| owner.token != token) {
        return Err(OrchError::NotAllowed(
            "another runtime owns this execution".into(),
        ));
    }
    Ok(())
}

impl Orchestrator {
    pub fn activate_automation(
        &self,
        id: &str,
        activation: &str,
        digest: &str,
    ) -> Result<automation::Definition, OrchError> {
        let mut state = self.inner.state.lock().unwrap();
        let (head, schedule) =
            self.inner
                .engine
                .store
                .transact_records(self.project().id.as_str(), |tx| {
                    let mut head: automation::Definition = serde_json::from_value(
                        tx.get_record(automation::DEFINITION, id)?
                            .ok_or_else(unavailable)?,
                    )?;
                    let mut prepared: automations::PreparedActivation = serde_json::from_value(
                        tx.get_record(automations::ACTIVATION, activation)?
                            .ok_or_else(unavailable)?,
                    )?;
                    if prepared.value.automation == id
                        && prepared.value.digest == digest
                        && prepared.value.phase == ActivationPhase::Active
                        && head.activation.as_deref() == Some(activation)
                        && head.active_revision == Some(prepared.value.revision)
                        && head.enabled
                    {
                        let schedule = head
                            .schedule
                            .as_ref()
                            .map(|id| -> Result<_, OrchError> {
                                Ok(serde_json::from_value::<ScheduleRecord>(
                                    tx.get_record(SCHEDULE, id)?.ok_or_else(unavailable)?,
                                )?)
                            })
                            .transpose()?;
                        return Ok::<_, OrchError>((head, schedule));
                    }
                    if prepared.value.automation != id
                        || prepared.value.revision != head.revision
                        || prepared.value.digest != digest
                        || prepared.value.phase == ActivationPhase::Revoked
                    {
                        return Err(OrchError::NotAllowed(
                            "the activation changed; review it again".into(),
                        ));
                    }
                    for (name, spec) in &prepared.observed {
                        let agent: AgentRecord = serde_json::from_value(
                            tx.get_record(AGENT, &format!("{}/{name}", self.project().id))?
                                .ok_or_else(|| OrchError::UnknownAgent(name.clone()))?,
                        )?;
                        if agent.state == AgentState::Terminated
                            || serde_json::to_value(&agent.spec)? != serde_json::to_value(spec)?
                        {
                            return Err(OrchError::NotAllowed(
                                "an agent changed after the activation preview".into(),
                            ));
                        }
                    }
                    let revision = automations::revision(tx, id, head.revision)?;
                    super::legacy_automation::validate_source(tx, &revision)?;
                    if prepared.value.computed_digest(&revision.digest)? != prepared.value.digest {
                        return Err(OrchError::NotAllowed(
                            "the activation no longer matches its revision".into(),
                        ));
                    }
                    if matches!(revision.draft.trigger, automation::Trigger::Webhook) {
                        let snapshot = prepared.value.webhook.as_ref().ok_or_else(|| {
                            OrchError::NotAllowed("the webhook connection is not verified".into())
                        })?;
                        let mut endpoint = super::webhooks::endpoint(tx, &snapshot.endpoint)?;
                        if head.webhook.as_deref() != Some(snapshot.endpoint.as_str())
                            || endpoint.draft_snapshot() != *snapshot
                            || endpoint.draft.availability
                                == automation::WebhookAvailability::Pending
                            || endpoint
                                .draft
                                .verified_ms
                                .is_none_or(|verified| now_ms().saturating_sub(verified) > 300_000)
                        {
                            return Err(OrchError::NotAllowed(
                                "the webhook connection changed before activation".into(),
                            ));
                        }
                        endpoint.active = Some(endpoint.draft.clone());
                        endpoint.active_since_ms = now_ms();
                        endpoint.enabled = true;
                        endpoint.updated_ms = now_ms();
                        tx.put_record(
                            automation::WEBHOOK,
                            &endpoint.id,
                            &serde_json::to_value(&endpoint)?,
                        )?;
                    } else if let Some(id) = &head.webhook {
                        let mut endpoint = super::webhooks::endpoint(tx, id)?;
                        endpoint.enabled = false;
                        endpoint.updated_ms = now_ms();
                        tx.put_record(automation::WEBHOOK, id, &serde_json::to_value(endpoint)?)?;
                    }
                    let new_activation =
                        head.activation.as_deref() != Some(activation) || !head.enabled;
                    let schedule = match (&head.schedule, &revision.draft.trigger) {
                        (existing, automation::Trigger::Schedule { when }) => {
                            if revision.draft.inputs.iter().any(|input| input.required) {
                                return Err(OrchError::NotAllowed(
                                    "scheduled inputs must be supplied by explicit step bindings"
                                        .into(),
                                ));
                            }
                            let mut record = match existing {
                                Some(existing) => serde_json::from_value::<ScheduleRecord>(
                                    tx.get_record(SCHEDULE, existing)?.ok_or_else(|| {
                                        OrchError::NotAllowed("the schedule is unavailable".into())
                                    })?,
                                )?,
                                None => ScheduleRecord::new(
                                    self.project().id.as_str(),
                                    "",
                                    TaskSpec::new(&revision.draft.name),
                                    when.clone(),
                                ),
                            };
                            if record.target.as_deref().is_some_and(|target| target != id) {
                                return Err(OrchError::NotAllowed(
                                    "this schedule belongs to another automation".into(),
                                ));
                            }
                            if record.paused {
                                record.failures = 0;
                                record.admission_error = None;
                                record.admission_failures = 0;
                                record.admission_retry_ms = None;
                            }
                            if record.when != *when || record.paused {
                                record.next_ms =
                                    when.next_after(now_ms().max(record.last_ms.unwrap_or(0)));
                            }
                            if record.next_ms.is_none() {
                                return Err(OrchError::NotAllowed(
                                    "the schedule has no future occurrence".into(),
                                ));
                            }
                            record.when = when.clone();
                            record.says = when.describe();
                            record.target = Some(id.into());
                            if new_activation {
                                record.target_since_ms = now_ms();
                            }
                            record.paused = false;
                            record.version = record.version.checked_add(1).ok_or_else(|| {
                                OrchError::NotAllowed(
                                    "the schedule version limit was reached".into(),
                                )
                            })?;
                            head.schedule = Some(record.id.clone());
                            tx.put_record(SCHEDULE, &record.id, &serde_json::to_value(&record)?)?;
                            Some(record)
                        }
                        (Some(existing), _) => {
                            let mut record: ScheduleRecord = serde_json::from_value(
                                tx.get_record(SCHEDULE, existing)?.ok_or_else(|| {
                                    OrchError::NotAllowed("the schedule is unavailable".into())
                                })?,
                            )?;
                            record.paused = true;
                            record.next_ms = None;
                            record.version = record.version.checked_add(1).ok_or_else(|| {
                                OrchError::NotAllowed(
                                    "the schedule version limit was reached".into(),
                                )
                            })?;
                            tx.put_record(SCHEDULE, &record.id, &serde_json::to_value(&record)?)?;
                            Some(record)
                        }
                        _ => None,
                    };
                    prepared.value.phase = ActivationPhase::Active;
                    head.enabled = true;
                    head.active_revision = Some(prepared.value.revision);
                    head.activation = Some(activation.into());
                    head.updated_ms = now_ms();
                    tx.put_record(
                        automations::ACTIVATION,
                        activation,
                        &serde_json::to_value(prepared)?,
                    )?;
                    tx.put_record(automation::DEFINITION, id, &serde_json::to_value(&head)?)?;
                    Ok::<_, OrchError>((head, schedule))
                })?;
        if let Some(schedule) = schedule {
            state.schedules.insert(schedule.id.clone(), schedule);
        }
        self.inner.emit(EventKind::Automation {
            automation: id.into(),
            run: None,
        });
        Ok(head)
    }

    pub fn pause_automation(&self, id: &str) -> Result<automation::Definition, OrchError> {
        let mut state = self.inner.state.lock().unwrap();
        let (head, schedule) =
            self.inner
                .engine
                .store
                .transact_records(self.project().id.as_str(), |tx| {
                    let mut head: automation::Definition = serde_json::from_value(
                        tx.get_record(automation::DEFINITION, id)?
                            .ok_or_else(unavailable)?,
                    )?;
                    head.enabled = false;
                    head.updated_ms = now_ms();
                    let schedule = match &head.schedule {
                        Some(id) => {
                            let mut schedule: ScheduleRecord = serde_json::from_value(
                                tx.get_record(SCHEDULE, id)?.ok_or_else(unavailable)?,
                            )?;
                            schedule.paused = true;
                            schedule.next_ms = None;
                            schedule.version =
                                schedule.version.checked_add(1).ok_or_else(|| {
                                    OrchError::NotAllowed(
                                        "the schedule version limit was reached".into(),
                                    )
                                })?;
                            tx.put_record(SCHEDULE, id, &serde_json::to_value(&schedule)?)?;
                            Some(schedule)
                        }
                        None => None,
                    };
                    tx.put_record(automation::DEFINITION, id, &serde_json::to_value(&head)?)?;
                    Ok::<_, OrchError>((head, schedule))
                })?;
        if let Some(schedule) = schedule {
            state.schedules.insert(schedule.id.clone(), schedule);
        }
        self.inner.emit(EventKind::Automation {
            automation: id.into(),
            run: None,
        });
        Ok(head)
    }

    pub fn revoke_automation_activation(&self, id: &str, digest: &str) -> Result<(), OrchError> {
        let mut state = self.inner.state.lock().unwrap();
        let (automation, schedule) =
            self.inner
                .engine
                .store
                .transact_records(self.project().id.as_str(), |tx| {
                    let mut prepared: automations::PreparedActivation = serde_json::from_value(
                        tx.get_record(automations::ACTIVATION, id)?
                            .ok_or_else(unavailable)?,
                    )?;
                    if prepared.value.digest != digest {
                        return Err(OrchError::NotAllowed(
                            "the grant changed; review it again".into(),
                        ));
                    }
                    prepared.value.phase = ActivationPhase::Revoked;
                    let automation = prepared.value.automation.clone();
                    tx.put_record(
                        automations::ACTIVATION,
                        id,
                        &serde_json::to_value(prepared)?,
                    )?;
                    let mut head: automation::Definition = serde_json::from_value(
                        tx.get_record(automation::DEFINITION, &automation)?
                            .ok_or_else(unavailable)?,
                    )?;
                    let mut schedule = None;
                    if head.activation.as_deref() == Some(id) {
                        head.enabled = false;
                        head.updated_ms = now_ms();
                        if let Some(id) = &head.schedule {
                            let mut record: ScheduleRecord = serde_json::from_value(
                                tx.get_record(SCHEDULE, id)?.ok_or_else(unavailable)?,
                            )?;
                            record.paused = true;
                            record.next_ms = None;
                            record.version = record.version.checked_add(1).ok_or_else(|| {
                                OrchError::NotAllowed(
                                    "the schedule version limit was reached".into(),
                                )
                            })?;
                            tx.put_record(SCHEDULE, id, &serde_json::to_value(&record)?)?;
                            schedule = Some(record);
                        }
                        if let Some(id) = &head.webhook {
                            let mut endpoint = super::webhooks::endpoint(tx, id)?;
                            endpoint.enabled = false;
                            endpoint.updated_ms = now_ms();
                            tx.put_record(
                                automation::WEBHOOK,
                                id,
                                &serde_json::to_value(endpoint)?,
                            )?;
                        }
                        tx.put_record(
                            automation::DEFINITION,
                            &automation,
                            &serde_json::to_value(head)?,
                        )?;
                    }
                    Ok::<_, OrchError>((automation, schedule))
                })?;
        if let Some(schedule) = schedule {
            state.schedules.insert(schedule.id.clone(), schedule);
        }
        drop(state);
        self.inner.emit(EventKind::Automation {
            automation,
            run: None,
        });
        Ok(())
    }

    pub fn begin_automation_start(
        &self,
        start: automation::ManualStart,
    ) -> Result<automation::ManualStart, OrchError> {
        let origin = RunOrigin::Manual {
            request: start.request.clone(),
        };
        origin.validate().map_err(OrchError::NotAllowed)?;
        let id = origin.run_id(self.project().id.as_str(), &start.automation)?;
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let head: automation::Definition = serde_json::from_value(
                    tx.get_record(automation::DEFINITION, &start.automation)?
                        .ok_or_else(unavailable)?,
                )?;
                if let Some(value) = tx.get_record(automation::RUN, &id)? {
                    let run: Run = serde_json::from_value(value)?;
                    if run.input_digest != origin.input_digest(&start.automation, start.expected, &start.inputs)? {
                        return Err(OrchError::NotAllowed("this start request names another execution".into()));
                    }
                    return Ok(start);
                }
                if let Some(prior) = tx.get_record(automation::MANUAL_START, &start.automation)? {
                    return Ok(serde_json::from_value(prior)?);
                }
                if head.revision != start.expected
                    || tx.get_record("automation_start_cancelled", &id)?.is_some()
                {
                    return Err(OrchError::NotAllowed(
                        "review the current revision and use a new start request".into(),
                    ));
                }
                let last: Option<automation::StartReceipt> = tx.get_record(automation::START_RECEIPT, &head.id)?.map(serde_json::from_value).transpose()?;
                if last.as_ref().map(|receipt| &receipt.run) != start.previous.as_ref() {
                    return Err(OrchError::NotAllowed("another execution already started; reopen this automation to review its last run".into()));
                }
                automations::revision(tx, &head.id, start.expected)?
                    .validate_inputs(&start.inputs)
                    .map_err(OrchError::NotAllowed)?;
                tx.insert_record(
                    automation::MANUAL_START,
                    &start.automation,
                    &serde_json::to_value(&start)?,
                )?;
                Ok(start)
            })
    }

    pub fn acknowledge_automation_start(
        &self,
        start: automation::ManualStart,
    ) -> Result<automation::StartReceipt, OrchError> {
        let origin = RunOrigin::Manual {
            request: start.request.clone(),
        };
        origin.validate().map_err(OrchError::NotAllowed)?;
        let id = origin.run_id(self.project().id.as_str(), &start.automation)?;
        let digest = origin.input_digest(&start.automation, start.expected, &start.inputs)?;
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let run: Run = serde_json::from_value(
                    tx.get_record(automation::RUN, &id)?
                        .ok_or_else(unavailable)?,
                )?;
                if run.input_digest != digest {
                    return Err(OrchError::NotAllowed(
                        "this start request names another execution".into(),
                    ));
                }
                let receipt = automation::StartReceipt {
                    request: start.clone(),
                    run: run.id,
                    work: run.work,
                };
                let pending: Option<automation::ManualStart> = tx
                    .get_record(automation::MANUAL_START, &start.automation)?
                    .map(serde_json::from_value)
                    .transpose()?;
                if pending.as_ref() == Some(&start)
                    || tx
                        .get_record(automation::START_RECEIPT, &start.automation)?
                        .is_none()
                {
                    tx.put_record(
                        automation::START_RECEIPT,
                        &start.automation,
                        &serde_json::to_value(&receipt)?,
                    )?;
                    if pending.as_ref() == Some(&start) {
                        tx.delete_record(automation::MANUAL_START, &start.automation)?;
                    }
                }
                Ok(receipt)
            })
    }

    pub fn cancel_automation_start(
        &self,
        automation: &str,
        expected: u64,
        request: &str,
        inputs: BTreeMap<String, InputValue>,
    ) -> Result<Option<Run>, OrchError> {
        let origin = RunOrigin::Manual {
            request: request.into(),
        };
        origin.validate().map_err(OrchError::NotAllowed)?;
        let id = origin.run_id(self.project().id.as_str(), automation)?;
        let digest = origin.input_digest(automation, expected, &inputs)?;
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                if tx.get_record(automation::DEFINITION, automation)?.is_none() {
                    return Err(unavailable());
                }
                let pending: Option<automation::ManualStart> = tx
                    .get_record(automation::MANUAL_START, automation)?
                    .map(serde_json::from_value)
                    .transpose()?;
                if let Some(pending) = pending.as_ref() {
                    if pending.request == request {
                        if pending.expected != expected || pending.inputs != inputs {
                            return Err(OrchError::NotAllowed(
                                "this start request names another execution".into(),
                            ));
                        }
                        tx.delete_record(automation::MANUAL_START, automation)?;
                    }
                }
                if let Some(value) = tx.get_record(automation::RUN, &id)? {
                    let run: Run = serde_json::from_value(value)?;
                    if run.input_digest != digest {
                        return Err(OrchError::NotAllowed(
                            "this start request names another execution".into(),
                        ));
                    }
                    if pending
                        .as_ref()
                        .is_some_and(|value| value.request == request)
                        || pending.is_none()
                            && tx
                                .get_record(automation::START_RECEIPT, automation)?
                                .is_none()
                    {
                        let start = pending.clone().unwrap_or_else(|| automation::ManualStart {
                            previous: None,
                            automation: automation.into(),
                            expected,
                            request: request.into(),
                            inputs: inputs.clone(),
                        });
                        tx.put_record(
                            automation::START_RECEIPT,
                            automation,
                            &serde_json::to_value(automation::StartReceipt {
                                request: start,
                                run: run.id.clone(),
                                work: run.work.clone(),
                            })?,
                        )?;
                    }
                    return Ok(Some(run));
                }
                if let Some(value) = tx.get_record("automation_start_cancelled", &id)? {
                    if value["digest"].as_str() != Some(digest.as_str()) {
                        return Err(OrchError::NotAllowed(
                            "this start request names another execution".into(),
                        ));
                    }
                    return Ok(None);
                }
                tx.insert_record(
                    "automation_start_cancelled",
                    &id,
                    &serde_json::json!({"digest":digest}),
                )?;
                Ok(None)
            })
    }

    pub async fn admit_manual_automation_run(
        &self,
        start: automation::ManualStart,
    ) -> Result<Run, OrchError> {
        if self.begin_automation_start(start.clone())? != start {
            return Err(OrchError::NotAllowed(
                "another start is pending; recover it before starting again".into(),
            ));
        }
        self.admit_automation_run(
            &start.automation,
            start.expected,
            RunOrigin::Manual {
                request: start.request,
            },
            start.inputs,
        )
        .await
    }

    pub async fn admit_automation_run(
        &self,
        automation: &str,
        expected: u64,
        origin: RunOrigin,
        inputs: BTreeMap<String, InputValue>,
    ) -> Result<Run, OrchError> {
        origin.validate().map_err(OrchError::NotAllowed)?;
        if matches!(origin, RunOrigin::Webhook { .. }) {
            return Err(OrchError::NotAllowed(
                "webhook delivery requires its verified connection".into(),
            ));
        }
        let id = origin.run_id(self.project().id.as_str(), automation)?;
        let input_digest = origin.input_digest(automation, expected, &inputs)?;
        let prior = self.inner.engine.store.transact_records::<_, OrchError>(
            self.project().id.as_str(),
            |tx| {
                tx.get_record(automation::RUN, &id)?
                    .map(serde_json::from_value::<Run>)
                    .transpose()
                    .map_err(Into::into)
            },
        )?;
        if let Some(prior) = prior {
            if prior.input_digest != input_digest {
                return Err(OrchError::NotAllowed(
                    "this trigger identity already names another execution".into(),
                ));
            }
            return Ok(prior);
        }
        let entry = self.automation(automation)?;
        let prepared = if matches!(origin, RunOrigin::Manual { .. }) {
            if entry.definition.revision != expected {
                return Err(OrchError::NotAllowed(
                    "the automation changed; review its current revision".into(),
                ));
            }
            let mut prepared = self
                .inner
                .prepare_automation_choices(&entry.revision)
                .await?;
            prepared.value.phase = ActivationPhase::Prepared;
            prepared
        } else {
            if !entry.definition.enabled || entry.definition.active_revision != Some(expected) {
                return Err(OrchError::NotAllowed(
                    "this automation is paused or its activation changed".into(),
                ));
            }
            self.inner.engine.store.transact_records::<_, OrchError>(
                self.project().id.as_str(),
                |tx| {
                    let prepared: automations::PreparedActivation = serde_json::from_value(
                        tx.get_record(
                            automations::ACTIVATION,
                            entry
                                .definition
                                .activation
                                .as_deref()
                                .ok_or_else(unavailable)?,
                        )?
                        .ok_or_else(unavailable)?,
                    )?;
                    if prepared.value.phase != ActivationPhase::Active {
                        return Err(OrchError::NotAllowed("this activation was revoked".into()));
                    }
                    Ok(prepared)
                },
            )?
        };
        self.inner.admit_frozen_automation_run(FrozenRunAdmission {
            automation,
            expected,
            origin,
            inputs,
            input_digest,
            prepared,
            due_now: None,
            authenticated_connection: None,
        })
    }
}

struct RunLease {
    inner: Arc<Inner>,
    run: String,
}
impl Drop for RunLease {
    fn drop(&mut self) {
        let result = self.inner.engine.store.transact_records::<_, OrchError>(
            self.inner.project.id.as_str(),
            |tx| {
                let mut run = load(tx, &self.run)?;
                if run
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.token == self.inner.execution_token)
                {
                    run.owner = None;
                    if !run.phase.is_final() && run.phase != RunPhase::WaitingApproval {
                        run.phase = RunPhase::Interrupted;
                        run.error =
                            Some("The execution stopped before its result was confirmed".into());
                    }
                    save(tx, &mut run)?;
                }
                Ok(())
            },
        );
        if let Err(error) = result {
            tracing::error!(run=%self.run, %error, "could not release automation execution ownership");
        }
        self.inner.cancel_automation_tasks(&self.run);
    }
}

impl Orchestrator {
    pub(super) fn recover_automations(&self) -> Result<(), OrchError> {
        let mut after = None;
        loop {
            let runs = self.inner.engine.store.transact_records::<_, OrchError>(
                self.project().id.as_str(),
                |tx| {
                    tx.records_states_after(
                        automation::RUN,
                        "phase",
                        &["pending", "running", "waiting_approval"],
                        after.as_deref(),
                        100,
                    )?
                    .into_iter()
                    .map(serde_json::from_value::<Run>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(Into::into)
                },
            )?;
            let Some(last) = runs.last() else {
                return Ok(());
            };
            after = Some(last.id.clone());
            for run in runs {
                self.execute_automation_run(&run.id)?;
            }
        }
    }

    pub fn execute_automation_run(&self, id: &str) -> Result<Run, OrchError> {
        let (run, acquired) =
            self.inner
                .engine
                .store
                .transact_records(self.project().id.as_str(), |tx| {
                    let mut run = load(tx, id)?;
                    let alive = run
                        .owner
                        .as_ref()
                        .is_some_and(|owner| process::writer_alive(owner.pid, owner.pid_start));
                    if run.phase.is_final() && run.phase != RunPhase::Interrupted || alive {
                        return Ok::<_, OrchError>((run, false));
                    }
                    for step in &mut run.steps {
                        if step.phase != StepPhase::Running {
                            continue;
                        }
                        let completed = if let Some(task) = &step.receipt {
                            let record: TaskRecord = serde_json::from_value(
                                tx.get_record(TASK, task)?.ok_or_else(unavailable)?,
                            )?;
                            let live = record.execution_owner.is_some()
                                && record.pid.is_some_and(|pid| {
                                    process::writer_alive(pid, record.pid_start)
                                });
                            !live
                                && (record.state == TaskState::Done
                                    || tx.get_record(results::RESULT, task)?.is_some())
                        } else {
                            let key = format!("{}/{}", run.id, step.id);
                            tx.get_record(TURN_RESULT, &key)?.is_some()
                        };
                        let pending = if completed {
                            false
                        } else {
                            let work = step
                                .receipt
                                .clone()
                                .unwrap_or_else(|| format!("{}/{}", run.id, step.id));
                            let calls = tx.execution_invocations(&step.scope, &work)?;
                            !calls.is_empty()
                                && calls.iter().all(|call| {
                                    call.status != ark_core::InvocationStatus::Running
                                        && !(call.risk == ark_core::RiskTier::Mutate
                                            && (call.status == ark_core::InvocationStatus::Failed
                                                || call.effect.as_ref().is_some_and(|effect| {
                                                    effect.state
                                                == ark_core::ExternalEffectState::Unconfirmed
                                                })))
                                })
                                && !tx
                                    .records_matching(TASK, "run", &run.id)?
                                    .into_iter()
                                    .map(serde_json::from_value::<TaskRecord>)
                                    .collect::<Result<Vec<_>, _>>()?
                                    .iter()
                                    .any(|task| {
                                        task.execution_step.as_deref() == Some(&step.id)
                                            && task.parent.is_some()
                                            && !task.state.is_final()
                                    })
                        };
                        if !completed && !pending {
                            run.phase = RunPhase::Interrupted;
                            run.error = Some(
                                "A step was interrupted. Review its saved result before continuing"
                                    .into(),
                            );
                            run.owner = None;
                            save(tx, &mut run)?;
                            return Ok((run, false));
                        }
                    }
                    run.owner = Some(RunOwner {
                        token: self.inner.execution_token.clone(),
                        pid: std::process::id(),
                        pid_start: process::own_start(),
                    });
                    run.phase = RunPhase::Running;
                    run.error = None;
                    save(tx, &mut run)?;
                    Ok((run, true))
                })?;
        if acquired {
            let inner = self.inner.clone();
            let id = id.to_string();
            tokio::spawn(async move {
                let _lease = RunLease {
                    inner: inner.clone(),
                    run: id.clone(),
                };
                if let Err(error) = inner.drive_automation(&id).await {
                    if let Err(storage) =
                        inner.finish_automation(&id, RunPhase::Failed, Some(error.to_string()))
                    {
                        tracing::error!(run=%id, %error, %storage, "could not retain automation failure");
                    }
                }
            });
        }
        Ok(run)
    }

    pub fn cancel_automation_run(&self, id: &str) -> Result<Run, OrchError> {
        let mut state = self.inner.state.lock().unwrap();
        let (run, item) =
            self.inner
                .engine
                .store
                .transact_records(self.project().id.as_str(), |tx| {
                    let mut run = load(tx, id)?;
                    let mut changed = None;
                    if !run.phase.is_final() {
                        run.cancel_requested = true;
                        let alive = run
                            .owner
                            .as_ref()
                            .is_some_and(|owner| process::writer_alive(owner.pid, owner.pid_start));
                        if !alive {
                            run.phase = RunPhase::Cancelled;
                            run.owner = None;
                            let mut item: WorkItem = serde_json::from_value(
                                tx.get_record(WORK, &run.work)?.ok_or_else(unavailable)?,
                            )?;
                            item.state = WorkState::Todo;
                            item.blocked = None;
                            item.done_ms = None;
                            item.updated_ms = now_ms();
                            tx.put_record(WORK, &item.id, &serde_json::to_value(&item)?)?;
                            changed = Some(item);
                        }
                        save(tx, &mut run)?;
                    }
                    Ok::<_, OrchError>((run, changed))
                })?;
        if let Some(item) = item {
            state.work.insert(item.id.clone(), item.clone());
            self.inner.emit(EventKind::Work {
                item,
                by: PERSON.into(),
            });
        }
        self.inner.emit(EventKind::Automation {
            automation: run.automation.clone(),
            run: Some(id.into()),
        });
        Ok(run)
    }

    pub fn answer_automation_question(
        &self,
        id: &str,
        question: &str,
        digest: &str,
        decision: ApprovalDecision,
    ) -> Result<Run, OrchError> {
        if !matches!(
            decision,
            ApprovalDecision::AllowOnce | ApprovalDecision::Deny
        ) {
            return Err(OrchError::NotAllowed(
                "an execution approval applies only to this question".into(),
            ));
        }
        let run = self
            .inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let mut run = load(tx, id)?;
                let prior = run
                    .steps
                    .iter()
                    .filter_map(|step| step.question.as_ref())
                    .chain(run.questions.values())
                    .find(|value| value.id == question && value.digest == digest)
                    .ok_or_else(|| {
                        OrchError::NotAllowed(
                            "the approval changed; reload it before answering".into(),
                        )
                    })?;
                super::automation_attention::retain_question(tx, &run, prior)?;
                if let Some(prior) = prior.decision {
                    return if prior == decision {
                        Ok(run)
                    } else {
                        Err(OrchError::NotAllowed(
                            "this question already has another answer".into(),
                        ))
                    };
                }
                if run.phase != RunPhase::WaitingApproval || run.cancel_requested {
                    return Err(OrchError::NotAllowed(
                        "this execution is no longer waiting for an answer".into(),
                    ));
                }
                let q = run
                    .steps
                    .iter_mut()
                    .filter_map(|step| step.question.as_mut())
                    .chain(run.questions.values_mut())
                    .find(|q| q.id == question && q.digest == digest)
                    .ok_or_else(|| {
                        OrchError::NotAllowed(
                            "the approval changed; reload it before answering".into(),
                        )
                    })?;
                if let Some(prior) = q.decision {
                    if prior != decision {
                        return Err(OrchError::NotAllowed(
                            "this question already has another answer".into(),
                        ));
                    }
                }
                q.decision = Some(decision);
                save(tx, &mut run)?;
                Ok::<_, OrchError>(run)
            })?;
        self.inner.emit(EventKind::Automation {
            automation: run.automation.clone(),
            run: Some(id.into()),
        });
        if !run
            .owner
            .as_ref()
            .is_some_and(|owner| process::writer_alive(owner.pid, owner.pid_start))
        {
            self.execute_automation_run(id)?;
        }
        Ok(run)
    }
}

fn bindings(run: &Run, inputs: &[automation::Binding]) -> Result<Output, OrchError> {
    use automation::{Binding, Port};
    let mut output = Output::default();
    for binding in inputs {
        let selected = match binding {
            Binding::Literal { text } => Output {
                text: text.clone(),
                files: vec![],
            },
            Binding::Artifact { id } => Output {
                text: String::new(),
                files: vec![ArtifactRef {
                    id: id.clone(),
                    name: String::new(),
                    kind: String::new(),
                }],
            },
            Binding::Input { name, .. } => match run.inputs.get(name) {
                Some(InputValue::Text { value }) => Output {
                    text: value.clone(),
                    files: vec![],
                },
                Some(InputValue::Files { ids }) => Output {
                    text: String::new(),
                    files: ids
                        .iter()
                        .map(|id| ArtifactRef {
                            id: id.clone(),
                            name: String::new(),
                            kind: String::new(),
                        })
                        .collect(),
                },
                None => Output::default(),
            },
            Binding::Step { step, port } => {
                let prior = run
                    .steps
                    .iter()
                    .find(|prior| &prior.id == step && prior.phase == StepPhase::Completed)
                    .and_then(|step| step.output.as_ref())
                    .ok_or_else(|| {
                        OrchError::NotAllowed("a bound step has no completed output".into())
                    })?;
                match port {
                    Port::Text => Output {
                        text: prior.text.clone(),
                        files: vec![],
                    },
                    Port::Files => Output {
                        text: String::new(),
                        files: prior.files.clone(),
                    },
                }
            }
        };
        if !selected.text.is_empty() {
            if !output.text.is_empty() {
                output.text.push_str("\n\n");
            }
            output.text.push_str(&selected.text);
        }
        for file in selected.files {
            if !output.files.iter().any(|known| known.id == file.id) {
                output.files.push(file);
            }
        }
        if output.text.len() > 256 * 1024 || output.files.len() > 128 {
            return Err(OrchError::NotAllowed(
                "the bound step input exceeds the execution limit".into(),
            ));
        }
    }
    Ok(output)
}

impl Inner {
    fn automation_bindings(
        &self,
        run: &Run,
        index: usize,
        inputs: &[automation::Binding],
    ) -> Result<Output, OrchError> {
        let mut output = bindings(run, inputs)?;
        self.engine
            .store
            .transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                for file in &mut output.files {
                    let mut sources = vec![run.project.as_str()];
                    sources.extend(run.steps[..index].iter().map(|step| step.scope.as_str()));
                    let mut found = None;
                    for source in sources {
                        if let Some(artifact) = tx.execution_artifact(source, &file.id)? {
                            found = Some(artifact);
                            break;
                        }
                    }
                    let artifact = found.ok_or_else(|| {
                        OrchError::NotAllowed("an explicitly bound file is unavailable".into())
                    })?;
                    *file = ArtifactRef {
                        id: artifact.id.to_string(),
                        name: artifact.name,
                        kind: artifact.kind.as_tag(),
                    };
                }
                Ok(())
            })?;
        Ok(output)
    }

    fn automation_change<T>(
        &self,
        id: &str,
        change: impl FnOnce(&mut Run, &ark_store::RecordTransaction<'_>) -> Result<T, OrchError>,
    ) -> Result<T, OrchError> {
        let (result, automation) =
            self.engine
                .store
                .transact_records(self.project.id.as_str(), |tx| {
                    let mut run = load(tx, id)?;
                    owned(&run, &self.execution_token)?;
                    let result = change(&mut run, tx)?;
                    save(tx, &mut run)?;
                    Ok::<_, OrchError>((result, run.automation))
                })?;
        self.emit(EventKind::Automation {
            automation,
            run: Some(id.into()),
        });
        Ok(result)
    }

    fn finish_automation(
        &self,
        id: &str,
        phase: RunPhase,
        error: Option<String>,
    ) -> Result<(), OrchError> {
        let mut state = self.state.lock().unwrap();
        let (item, settled, schedule) =
            self.engine
                .store
                .transact_records(self.project.id.as_str(), |tx| {
                    let mut run = load(tx, id)?;
                    owned(&run, &self.execution_token)?;
                    run.phase = if run.cancel_requested {
                        RunPhase::Cancelled
                    } else {
                        phase
                    };
                    run.error = if run.cancel_requested { None } else { error };
                    if let Some(step) = run
                        .steps
                        .iter_mut()
                        .find(|step| step.phase == StepPhase::Running)
                    {
                        step.phase = if run.cancel_requested {
                            StepPhase::Cancelled
                        } else {
                            StepPhase::Failed
                        };
                        step.error = run.error.clone();
                    }
                    let mut settled = Vec::new();
                    for value in tx.records_matching(TASK, "run", id)? {
                        let mut task: TaskRecord = serde_json::from_value(value)?;
                        let live = task.execution_owner.is_some()
                            && task
                                .pid
                                .is_some_and(|pid| process::writer_alive(pid, task.pid_start));
                        if !task.state.is_final() && !live {
                            task.state = if run.phase == RunPhase::Cancelled {
                                TaskState::Cancelled
                            } else {
                                TaskState::Failed
                            };
                            task.error = run.error.clone().or_else(|| {
                                Some("the parent execution ended before this task started".into())
                            });
                            task.finished_ms = Some(now_ms());
                            task.execution_owner = None;
                            task.pid = None;
                            task.pid_start = None;
                            tx.put_record(TASK, &task.id, &serde_json::to_value(&task)?)?;
                            settled.push(task);
                        }
                    }
                    let mut item: WorkItem = serde_json::from_value(
                        tx.get_record(WORK, &run.work)?.ok_or_else(unavailable)?,
                    )?;
                    item.state = if run.phase == RunPhase::Completed {
                        WorkState::Done
                    } else {
                        WorkState::Todo
                    };
                    item.done_ms = (run.phase == RunPhase::Completed).then(now_ms);
                    item.blocked = run.error.clone();
                    item.updated_ms = now_ms();
                    tx.put_record(WORK, &item.id, &serde_json::to_value(&item)?)?;
                    automation_reports::prepare(tx, &run)?;
                    let schedule = automation_schedule::settle(tx, &run)?;
                    save(tx, &mut run)?;
                    Ok::<_, OrchError>((item, settled, schedule))
                })?;
        for task in settled {
            if let Some(current) = state.tasks.get_mut(&task.id) {
                current.record = task;
                current.done.send_replace(true);
            }
        }
        if let Some(schedule) = schedule {
            state.schedules.insert(schedule.id.clone(), schedule);
        }
        state.work.insert(item.id.clone(), item.clone());
        drop(state);
        self.cancel_automation_tasks(id);
        if let Err(error) = self.deliver_automation_reports() {
            tracing::error!(run=%id,%error,"could not settle automation result delivery");
        }
        self.emit(EventKind::Work {
            item,
            by: PERSON.into(),
        });
        Ok(())
    }

    async fn drive_automation(self: &Arc<Self>, id: &str) -> Result<(), OrchError> {
        let guard = self.run_guard(id, true)?;
        loop {
            guard.ensure_current()?;
            let run = Orchestrator {
                inner: self.clone(),
            }
            .automation_run(id)?;
            if run.cancel_requested {
                return self.finish_automation(id, RunPhase::Cancelled, None);
            }
            for (index, step) in run.steps.iter().enumerate() {
                if step.phase == StepPhase::Completed {
                    self.publish_automation_step(id, index)?;
                }
            }
            let Some(index) = run
                .steps
                .iter()
                .position(|step| step.phase != StepPhase::Completed)
            else {
                return self.finish_automation(id, RunPhase::Completed, None);
            };
            let step = &run.revision.draft.steps[index];
            let input = self.automation_bindings(&run, index, step.action.inputs())?;
            match &step.action {
                automation::StepAction::Approval { title, .. } => {
                    let question = &run.steps[index].question;
                    match question.as_ref().and_then(|question| question.decision) {
                        Some(ApprovalDecision::Deny) => {
                            return self.finish_automation(id, RunPhase::Cancelled, None)
                        }
                        Some(_) => self.automation_change(id, |run, _| {
                            let question =
                                run.steps[index].question.as_ref().ok_or_else(unavailable)?;
                            let approved = Output {
                                text: question.detail.clone(),
                                files: question.files.clone(),
                            };
                            let digest = format!(
                                "v1:{}",
                                blake3::hash(&serde_json::to_vec(&(
                                    &run.revision.digest,
                                    &step.id,
                                    &approved
                                ))?)
                                .to_hex()
                            );
                            if digest != question.digest {
                                return Err(OrchError::NotAllowed(
                                    "the approved values changed".into(),
                                ));
                            }
                            run.steps[index].phase = StepPhase::Completed;
                            run.steps[index].output = Some(approved);
                            run.phase = RunPhase::Running;
                            Ok(())
                        })?,
                        None => {
                            self.automation_change(id, |run, tx| {
                                if run.steps[index].question.is_none() {
                                    let digest = format!(
                                        "v1:{}",
                                        blake3::hash(&serde_json::to_vec(&(
                                            &run.revision.digest,
                                            &step.id,
                                            &input
                                        ))?)
                                        .to_hex()
                                    );
                                    run.steps[index].question = Some(automation::RunQuestion {
                                        id: format!("aq-{}", uuid::Uuid::new_v4().simple()),
                                        title: title.clone(),
                                        detail: input.text.clone(),
                                        digest,
                                        decision: None,
                                        action: None,
                                        files: input.files.clone(),
                                    });
                                }
                                super::automation_attention::retain_question(
                                    tx,
                                    run,
                                    run.steps[index].question.as_ref().ok_or_else(unavailable)?,
                                )?;
                                run.steps[index].phase = StepPhase::WaitingApproval;
                                run.phase = RunPhase::WaitingApproval;
                                Ok(())
                            })?;
                            loop {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                let observed = Orchestrator {
                                    inner: self.clone(),
                                }
                                .automation_run(id)?;
                                if observed.cancel_requested {
                                    return self.finish_automation(id, RunPhase::Cancelled, None);
                                }
                                if observed.steps[index]
                                    .question
                                    .as_ref()
                                    .and_then(|question| question.decision)
                                    .is_some()
                                {
                                    break;
                                }
                            }
                            continue;
                        }
                    }
                }
                automation::StepAction::Result { .. } => {
                    self.automation_change(id, |run, _| {
                        run.steps[index].output = Some(input.clone());
                        run.steps[index].phase = StepPhase::Completed;
                        run.output = input;
                        Ok(())
                    })?;
                }
                automation::StepAction::Agent { target, prompt, .. } => {
                    let selected = run.selections.get(&step.id).ok_or_else(|| {
                        OrchError::NotAllowed("the step's frozen model is missing".into())
                    })?;
                    let output = if matches!(target, WorkTarget::Agent { .. }) {
                        self.run_automation_agent(&run, index, target, prompt, input, selected)
                            .await?
                    } else {
                        self.run_automation_turn(
                            &run,
                            index,
                            prompt,
                            input,
                            selected,
                            guard.clone(),
                        )
                        .await?
                    };
                    self.automation_change(id, |run, _| {
                        run.steps[index].output = Some(output.clone());
                        run.steps[index].phase = StepPhase::Completed;
                        run.output = output;
                        Ok(())
                    })?;
                    if let Some(error) = guard.fault() {
                        return Err(OrchError::NotAllowed(error));
                    }
                }
            }
            self.publish_automation_step(id, index)?;
        }
    }
}

impl Inner {
    async fn run_automation_turn(
        self: &Arc<Self>,
        run: &Run,
        index: usize,
        prompt: &str,
        input: Output,
        selected: &automation::FrozenSelection,
        guard: Arc<authority::RunGuard>,
    ) -> Result<Output, OrchError> {
        let step = &run.steps[index];
        let scope = ProjectId(step.scope.clone());
        let mut session = Session::open_for_execution(&self.engine, &step.conversation)?
            .ok_or_else(unavailable)?;
        if session.record().project != scope {
            return Err(OrchError::NotAllowed(
                "the step conversation changed projects".into(),
            ));
        }
        let key = format!("{}/{}", run.id, step.id);
        let mut message = prompt.to_string();
        if !input.text.is_empty() {
            message.push_str("\n\n");
            message.push_str(&input.text);
        }
        for file in &input.files {
            let artifact = self.engine.store.transact_records::<_, OrchError>(
                self.project.id.as_str(),
                |tx| {
                    let mut sources = vec![run.project.as_str()];
                    sources.extend(run.steps[..index].iter().map(|step| step.scope.as_str()));
                    for source in sources {
                        if let Some(artifact) = tx.execution_artifact(source, &file.id)? {
                            return Ok(artifact);
                        }
                    }
                    Err(OrchError::NotAllowed(
                        "an explicitly bound file is unavailable".into(),
                    ))
                },
            )?;
            self.engine.store.put_artifact(&scope, &artifact)?;
            message.push_str(&format!(
                "\nart:{} {} ({})",
                artifact.id,
                artifact.name,
                artifact.kind.as_tag()
            ));
        }
        let saved =
            self.engine
                .store
                .transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                    tx.get_record(TURN_RESULT, &key)?
                        .map(serde_json::from_value::<results::SavedResult>)
                        .transpose()
                        .map_err(Into::into)
                })?;
        if let Some(saved) = saved {
            if saved.conversation != step.conversation || saved.index != 0 || saved.input != message
            {
                return Err(OrchError::NotAllowed(
                    "the retained step result does not match its admitted input".into(),
                ));
            }
            let outcome = saved.outcome();
            if session.turn_count() == saved.index {
                session.record_turn(&self.engine, &message, &outcome)?;
            } else {
                let turn = self
                    .engine
                    .store
                    .transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                        Ok(tx.get_execution_turn(&step.conversation, saved.index)?)
                    })?
                    .ok_or_else(unavailable)?;
                if session.turn_count() != 1
                    || turn["input"] != saved.input
                    || turn["answer"] != saved.answer
                {
                    return Err(OrchError::NotAllowed(
                        "the retained step result conflicts with its conversation".into(),
                    ));
                }
            }
            return self.automation_turn_output(&scope, &key, &outcome);
        }
        if session.turn_count() != 0 {
            return Err(OrchError::NotAllowed(
                "the step already has a turn without its execution receipt".into(),
            ));
        }
        let cancel = CancelToken::new();
        let monitor_cancel = cancel.clone();
        let monitor_inner = self.clone();
        let monitor_run = run.id.clone();
        let monitor_guard = guard.clone();
        let monitor = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let Err(error) = monitor_guard.ensure_current() {
                    monitor_guard.fail(error.to_string());
                    monitor_inner.cancel_automation_tasks(&monitor_run);
                    monitor_cancel.cancel();
                    return;
                }
                let observed = Orchestrator {
                    inner: monitor_inner.clone(),
                }
                .automation_run(&monitor_run);
                match observed {
                    Ok(run) if !run.cancel_requested && !run.phase.is_final() => {}
                    _ => {
                        monitor_inner.cancel_automation_tasks(&monitor_run);
                        monitor_cancel.cancel();
                        return;
                    }
                }
            }
        });
        let result = self
            .automation_turn_with_models(
                GeneralTurn {
                    run,
                    index,
                    selected,
                    session: &mut session,
                    message: &message,
                },
                guard,
                cancel,
            )
            .await;
        monitor.abort();
        result
    }

    async fn automation_turn_with_models(
        self: &Arc<Self>,
        turn: GeneralTurn<'_>,
        guard: Arc<authority::RunGuard>,
        cancel: CancelToken,
    ) -> Result<Output, OrchError> {
        let GeneralTurn {
            run,
            index,
            selected,
            ..
        } = &turn;
        let step = &run.steps[*index];
        let lane = format!("{}/automation/{}/{}", self.project.id, run.id, step.id);
        let resolved = self.models.resolve(&lane, Some(&selected.model)).await;
        let result = match resolved {
            Ok(models) if self.models.selection_spec(&models) == selected.model => {
                self.automation_turn_with_lane(turn, guard, cancel, &models)
                    .await
            }
            Ok(_) => Err(OrchError::NotAllowed(
                "the frozen model selection is unavailable".into(),
            )),
            Err(error) => Err(error),
        };
        self.models.close(&lane).await;
        result
    }

    async fn automation_turn_with_lane(
        self: &Arc<Self>,
        turn: GeneralTurn<'_>,
        guard: Arc<authority::RunGuard>,
        cancel: CancelToken,
        models: &AgentModels,
    ) -> Result<Output, OrchError> {
        let GeneralTurn {
            run,
            index,
            session,
            message,
            ..
        } = turn;
        models.require_host_tools()?;
        let step = &run.steps[index];
        let scope = ProjectId(step.scope.clone());
        let key = format!("{}/{}", run.id, step.id);
        let account = self.account_slots(&models.account);
        let _account = tokio::select! { permit = account.acquire_owned() => permit.map_err(|_| OrchError::NotAllowed("the model account closed".into()))?, _ = cancel.cancelled() => return Err(OrchError::NotAllowed("the execution was cancelled".into())) };
        let _global = tokio::select! { permit = self.global.clone().acquire_owned() => permit.map_err(|_| OrchError::NotAllowed("execution capacity closed".into()))?, _ = cancel.cancelled() => return Err(OrchError::NotAllowed("the execution was cancelled".into())) };
        let capacity = Arc::new(tokio::sync::Mutex::new(Slots {
            account: Some(_account),
            global: Some(_global),
            account_name: models.account.clone(),
            waiters: 0,
        }));
        let mut registry = self
            .engine
            .tools()
            .filtered(|action| !action.starts_with("agents."));
        registry.register(Arc::new(super::automation_children::RunTools {
            inner: self.clone(),
            run: run.clone(),
            step: step.id.clone(),
            parent: None,
            capacity: Some(capacity),
        }));
        let mut profile = self.base.clone();
        let owner = format!("{}/{}/{}", self.project.id, run.id, step.id);
        for per_agent in &self.per_agent {
            let adapter = per_agent.for_owner(&ark_runtime::per_agent::Owner {
                name: &owner,
                model: models.router.provider(ark_runtime::Role::Plan),
            });
            for adapter in Registry::new().with(adapter).adapters() {
                registry.register(adapter.clone());
            }
            if let Some(distiller) = per_agent.distiller() {
                profile.distillers.push(distiller);
            }
        }
        let engine = self
            .engine
            .for_work(&key)
            .scoped(registry)
            .guarded(guard.clone());
        let gate = Arc::new(ApprovalGate::new(Arc::new(RunApprover {
            inner: self.clone(),
            run: run.id.clone(),
            index,
            task: None,
            guard: guard.clone(),
        })));
        self.automation_change(&run.id, |current, _| {
            if current.cancel_requested {
                return Err(OrchError::NotAllowed("the execution was cancelled".into()));
            }
            current.steps[index].phase = StepPhase::Running;
            Ok(())
        })?;
        session.begin_turn(&engine, message)?;
        let done = self
            .resume_automation_actions(&engine, &scope, &key, &gate, &guard, &cancel)
            .await?;
        let harness = Harness::new(models.router.clone_view())
            .max_steps(self.max_steps)
            .budget_or_auto(self.context_budget)
            .planner(self.planner)
            .critic(self.critic)
            .guard(guard.clone())
            .done_before(done)
            .chat(run.id.as_str())
            .approvals(Some(gate))
            .cancel(Some(cancel))
            .stopping(Some(engine.stopping()));
        guard.ensure_current()?;
        let outcome = harness
            .ask(&engine, &profile, &scope, message, |_| {})
            .await?;
        let saved = results::SavedResult::capture(session, message, &outcome);
        self.automation_change(&run.id, |current, tx| {
            tx.insert_record(TURN_RESULT, &key, &serde_json::to_value(&saved)?)?;
            if !outcome.answer.is_empty() {
                current.output.text = outcome.answer.clone();
            }
            Ok(())
        })?;
        session.record_turn(&engine, message, &outcome)?;
        if let Some(error) = guard.fault() {
            return Err(OrchError::NotAllowed(error));
        }
        if outcome.cancelled || outcome.interrupted || outcome.ran_out {
            return Err(OrchError::NotAllowed(
                "the step stopped before completion; its partial result is retained".into(),
            ));
        }
        self.automation_turn_output(&scope, &key, &outcome)
    }

    fn automation_turn_output(
        &self,
        scope: &ProjectId,
        key: &str,
        outcome: &ark_runtime::HarnessOutcome,
    ) -> Result<Output, OrchError> {
        let mut output = Output {
            text: outcome.answer.clone(),
            files: vec![],
        };
        for invocation in self.engine.work_calls(scope, key)? {
            for id in invocation.produced {
                let artifact =
                    self.engine.store.get_artifact(scope, &id)?.ok_or_else(|| {
                        OrchError::NotAllowed("a produced file is missing".into())
                    })?;
                if artifact
                    .meta
                    .get("before")
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
                    && !output.files.iter().any(|file| file.id == id.as_str())
                {
                    output.files.push(ArtifactRef {
                        id: id.to_string(),
                        name: artifact.name,
                        kind: artifact.kind.as_tag(),
                    });
                }
            }
        }
        Ok(output)
    }

    fn publish_automation_step(&self, id: &str, index: usize) -> Result<(), OrchError> {
        self.engine
            .store
            .transact_records(self.project.id.as_str(), |tx| {
                let run = load(tx, id)?;
                owned(&run, &self.execution_token)?;
                let key = format!("{id}/{}", run.steps[index].id);
                if tx.get_record("automation_publication", &key)?.is_some() {
                    return Ok::<_, OrchError>(());
                }
                if let Some(output) = &run.steps[index].output {
                    for file in &output.files {
                        let mut sources = vec![run.project.as_str()];
                        sources.extend(run.steps[..=index].iter().map(|step| step.scope.as_str()));
                        let mut found = None;
                        for source in sources {
                            if let Some(artifact) = tx.execution_artifact(source, &file.id)? {
                                found = Some(artifact);
                                break;
                            }
                        }
                        tx.put_artifact(&found.ok_or_else(|| {
                            OrchError::NotAllowed("a public output file is unavailable".into())
                        })?)?;
                    }
                }
                let mut header: SessionRecord = serde_json::from_value(
                    tx.get_session(&run.conversation)?.ok_or_else(unavailable)?,
                )?;
                let turn = ark_runtime::session::Turn {
                    index: header.turn_count,
                    input: match &run.revision.draft.steps[index].action {
                        automation::StepAction::Agent { prompt, inputs, .. } => {
                            let bound = bindings(&run, inputs)?;
                            if bound.text.is_empty() {
                                prompt.clone()
                            } else {
                                format!("{prompt}\n\n{}", bound.text)
                            }
                        }
                        automation::StepAction::Approval { title, .. } => title.clone(),
                        automation::StepAction::Result { .. } => {
                            run.revision.draft.steps[index].name.clone()
                        }
                    },
                    answer: run.steps[index]
                        .output
                        .as_ref()
                        .map(|output| output.text.clone())
                        .unwrap_or_else(|| "Approved".into()),
                    cited: vec![],
                    cancelled: false,
                    steered: vec![],
                    images: vec![],
                    metrics: Default::default(),
                    at: time::OffsetDateTime::now_utc(),
                    incomplete: false,
                    error: None,
                    pid: None,
                    pid_start: None,
                    goal: None,
                    ran_out: false,
                    execution_mode: Default::default(),
                };
                tx.put_session_turn(
                    &run.conversation,
                    header.turn_count,
                    &serde_json::to_value(&turn)?,
                )?;
                header.turn_count = header.turn_count.checked_add(1).ok_or_else(|| {
                    OrchError::NotAllowed("the conversation turn limit was reached".into())
                })?;
                header.updated_at = turn.at;
                tx.put_session(&run.conversation, &serde_json::to_value(&header)?)?;
                tx.insert_record(
                    "automation_publication",
                    &key,
                    &serde_json::json!({"index":turn.index}),
                )?;
                Ok(())
            })
    }
}

struct RunApprover {
    guard: Arc<authority::RunGuard>,
    inner: Arc<Inner>,
    run: String,
    index: usize,
    task: Option<String>,
}
#[async_trait::async_trait]
impl Approver for RunApprover {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let result = self.inner.automation_change(&self.run, |run, tx| {
            let mut request = request.clone();
            if let Some(task) = &self.task {
                let task: TaskRecord = serde_json::from_value(
                    tx.get_record(TASK, task)?
                        .ok_or_else(|| OrchError::UnknownTask(task.clone()))?,
                )?;
                if task.run.as_deref() != Some(&self.run)
                    || task.execution_step.as_deref() != Some(&run.steps[self.index].id)
                    || task.execution_owner.as_deref() != Some(&self.inner.execution_token)
                {
                    return Err(OrchError::NotAllowed(
                        "the approval task changed execution ownership".into(),
                    ));
                }
                if request.project.as_ref().map(|scope| scope.as_str())
                    != task.execution_scope.as_deref()
                {
                    return Err(OrchError::NotAllowed(
                        "the approval belongs to another task context".into(),
                    ));
                }
                request.agent = Some(task.owner);
                request.task = Some(task.id);
            }
            if run.cancel_requested {
                return Err(OrchError::NotAllowed("the execution was cancelled".into()));
            }
            self.guard.verify_pending_approval(tx, run, &request)?;
            if let Some(question) = run.questions.values().find(|question| {
                question
                    .action
                    .as_ref()
                    .is_some_and(|action| action.invocation == request.invocation)
            }) {
                let retained = question.action.as_ref().ok_or_else(unavailable)?;
                self.guard.verify_pending_approval(tx, run, retained)?;
                if retained.project != request.project
                    || retained.task != request.task
                    || retained.agent != request.agent
                {
                    return Err(OrchError::NotAllowed(
                        "the retained approval belongs to another execution context".into(),
                    ));
                }
                let digest = format!(
                    "v1:{}",
                    blake3::hash(&serde_json::to_vec(&(
                        &run.revision.digest,
                        &run.steps[self.index].id,
                        retained
                    ))?)
                    .to_hex()
                );
                if question.digest != digest {
                    return Err(OrchError::NotAllowed(
                        "the retained approval changed".into(),
                    ));
                }
                let id = question.id.clone();
                run.phase = RunPhase::WaitingApproval;
                return Ok(id);
            }
            let digest = format!(
                "v1:{}",
                blake3::hash(&serde_json::to_vec(&(
                    &run.revision.digest,
                    &run.steps[self.index].id,
                    &request
                ))?)
                .to_hex()
            );
            let question = automation::RunQuestion {
                id: format!("aq-{}", uuid::Uuid::new_v4().simple()),
                title: request.preview.clone(),
                detail: request.summary.clone(),
                digest,
                decision: None,
                action: Some(request.clone()),
                files: vec![],
            };
            let id = question.id.clone();
            if run.questions.len() >= 1000 {
                return Err(OrchError::NotAllowed(
                    "this execution's approval limit was reached".into(),
                ));
            }
            super::automation_attention::retain_question(tx, run, &question)?;
            run.questions.insert(id.clone(), question);
            run.phase = RunPhase::WaitingApproval;
            Ok(id)
        });
        let id = match result {
            Ok(id) => id,
            Err(error) => {
                self.guard
                    .fail(format!("could not retain automation approval: {error}"));
                tracing::error!(run=%self.run, %error, "could not retain automation approval");
                return ApprovalDecision::Deny;
            }
        };
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Err(error) = self.guard.ensure_current() {
                self.guard.fail(error.to_string());
                return ApprovalDecision::Deny;
            }
            let observed = Orchestrator {
                inner: self.inner.clone(),
            }
            .automation_run(&self.run);
            match observed {
                Ok(run) if !run.cancel_requested && !run.phase.is_final() => {
                    if let Some(decision) = run
                        .questions
                        .get(&id)
                        .and_then(|question| question.decision)
                    {
                        let ready = self.inner.automation_change(&self.run, |run, _| {
                            let pending = run
                                .questions
                                .values()
                                .chain(run.steps.iter().filter_map(|step| step.question.as_ref()))
                                .any(|question| question.decision.is_none());
                            if !pending {
                                run.phase = RunPhase::Running;
                            }
                            Ok(!pending)
                        });
                        if matches!(ready, Ok(false)) {
                            continue;
                        }
                        if let Err(error) = ready {
                            self.guard.fail(format!(
                                "could not retain automation approval settlement: {error}"
                            ));
                            tracing::error!(run=%self.run, %error, "could not retain automation approval settlement");
                            return ApprovalDecision::Deny;
                        }
                        return decision;
                    }
                }
                Ok(_) => return ApprovalDecision::Deny,
                Err(error) => {
                    self.guard
                        .fail(format!("could not read automation approval: {error}"));
                    tracing::error!(run=%self.run, %error, "could not read automation approval");
                    return ApprovalDecision::Deny;
                }
            }
        }
    }
}

impl Inner {
    pub(super) async fn resume_automation_actions(
        &self,
        engine: &Engine,
        scope: &ProjectId,
        work: &str,
        gate: &ApprovalGate,
        guard: &authority::RunGuard,
        cancel: &CancelToken,
    ) -> Result<Vec<ark_core::Invocation>, OrchError> {
        guard.ensure_current()?;
        for call in engine.work_calls(scope, work)? {
            if call.status == ark_core::InvocationStatus::Running {
                return Err(OrchError::NotAllowed(
                    "an earlier action has no confirmed result; review it before continuing".into(),
                ));
            }
            if call.status == ark_core::InvocationStatus::PendingApproval {
                tokio::select! {
                    result = engine.run_pending_with_approval(scope, &call.id, gate) => {
                        if result?.status == ark_core::InvocationStatus::Denied {
                            return Err(OrchError::NotAllowed("the reviewed action was declined".into()));
                        }
                    },
                    _ = cancel.cancelled() => return Err(OrchError::NotAllowed("the execution was cancelled".into())),
                }
                guard.ensure_current()?;
            }
        }
        Ok(engine.work_calls(scope, work)?)
    }

    pub(super) fn automation_approvals(
        &self,
        id: &str,
        task: &str,
    ) -> Result<Arc<ApprovalGate>, OrchError> {
        let (run, task_record) =
            self.engine
                .store
                .transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                    Ok((
                        load(tx, id)?,
                        serde_json::from_value::<TaskRecord>(
                            tx.get_record(TASK, task)?
                                .ok_or_else(|| OrchError::UnknownTask(task.into()))?,
                        )?,
                    ))
                })?;
        let index = run
            .steps
            .iter()
            .position(|step| {
                step.receipt.as_deref() == Some(task)
                    || task_record.run.as_deref() == Some(id)
                        && task_record.execution_step.as_deref() == Some(&step.id)
            })
            .ok_or_else(|| {
                OrchError::NotAllowed("the task is not this execution's current step".into())
            })?;
        Ok(Arc::new(ApprovalGate::new(Arc::new(RunApprover {
            inner: self.arc(),
            run: id.into(),
            index,
            task: Some(task.into()),
            guard: self.run_guard(id, false)?,
        }))))
    }

    async fn run_automation_agent(
        self: &Arc<Self>,
        run: &Run,
        index: usize,
        target: &WorkTarget,
        prompt: &str,
        input: Output,
        selected: &automation::FrozenSelection,
    ) -> Result<Output, OrchError> {
        let spec = selected
            .agent
            .as_ref()
            .ok_or_else(|| OrchError::NotAllowed("the step has no frozen agent".into()))?;
        let receipt_id = format!(
            "wr-{}",
            blake3::hash(format!("{}/{}", run.id, run.steps[index].id).as_bytes()).to_hex()
        );
        let task_id = format!("t-{}", blake3::hash(receipt_id.as_bytes()).to_hex());
        self.run_guard(&run.id, false)?.ensure_current()?;
        self.automation_change(&run.id, |current, tx| {
            if current.cancel_requested {
                return Err(OrchError::NotAllowed("the execution was cancelled".into()));
            }
            if tx.get_record(admission::SUBMISSION, &receipt_id)?.is_some() {
                return Ok(());
            }
            let mut task_spec = TaskSpec::new(prompt);
            task_spec.context = input.text.clone();
            task_spec.artifacts = input.files.iter().map(|file| file.id.clone()).collect();
            task_spec.item = Some(run.work.clone());
            task_spec.max_attempts = Some(1);
            if let Some(policies) = run
                .revision
                .legacy
                .as_ref()
                .filter(|policies| policies.step == run.steps[index].id)
            {
                task_spec.after = policies.after.clone();
                task_spec.budget = policies.budget.clone();
                task_spec.rules = policies.rules.clone();
                task_spec.work = policies.work.clone();
                task_spec.max_attempts = Some(policies.max_attempts);
                task_spec.retry_delay_ms = Some(policies.retry_delay_ms);
                for dependency in &task_spec.after {
                    if tx.get_record(TASK, dependency)?.is_none() {
                        return Err(OrchError::UnknownTask(dependency.clone()));
                    }
                }
            }
            let mut artifacts = Vec::new();
            for file in &input.files {
                let mut sources = vec![run.project.as_str()];
                sources.extend(run.steps[..index].iter().map(|step| step.scope.as_str()));
                let mut found = None;
                for source in sources {
                    if let Some(artifact) = tx.execution_artifact(source, &file.id)? {
                        found = Some(artifact);
                        break;
                    }
                }
                artifacts.push(found.ok_or_else(|| {
                    OrchError::NotAllowed("an explicitly bound file is unavailable".into())
                })?);
            }
            let inputs = artifacts
                .iter()
                .map(|artifact| ArtifactRef {
                    id: artifact.id.to_string(),
                    name: artifact.name.clone(),
                    kind: artifact.kind.as_tag(),
                })
                .collect();
            let mut task = TaskRecord::queued(
                task_id.clone(),
                run.project.clone(),
                spec.name.clone(),
                &task_spec,
                Origin::External,
                task_spec
                    .budget
                    .clone()
                    .unwrap_or_default()
                    .or(&spec.budget),
                inputs,
            );
            task.conversation = Some(run.steps[index].conversation.clone());
            task.execution_scope = Some(run.steps[index].scope.clone());
            task.run = Some(run.id.clone());
            task.execution_step = Some(run.steps[index].id.clone());
            task.submission = Some(receipt_id.clone());
            task.item = Some(run.work.clone());
            let submission = admission::Submission {
                receipt: WorkReceipt {
                    id: receipt_id.clone(),
                    project: run.project.clone(),
                    origin: "automation-step".into(),
                    request: format!("{}/{}", run.id, run.steps[index].id),
                    digest: run.revision.digest.clone(),
                    conversation: run.steps[index].conversation.clone(),
                    work: run.work.clone(),
                    task: Some(task_id.clone()),
                    turn: None,
                    model: selected.model.clone(),
                    created_ms: now_ms(),
                    sequence: 0,
                    parent: Some(WorkRunRef::Automation {
                        run: run.id.clone(),
                    }),
                },
                target: target.clone(),
                rules: task_spec.rules.clone(),
                spec: task_spec,
                artifacts,
                agent: Some(spec.clone()),
            };
            tx.insert_record(TASK, &task_id, &serde_json::to_value(task)?)?;
            tx.insert_record(
                admission::SUBMISSION,
                &receipt_id,
                &serde_json::to_value(submission)?,
            )?;
            current.steps[index].receipt = Some(task_id.clone());
            current.steps[index].phase = StepPhase::Running;
            Ok(())
        })?;
        let o = Orchestrator {
            inner: self.clone(),
        };
        o.execute_work(&receipt_id)?;
        loop {
            let current = o.automation_run(&run.id)?;
            if current.cancel_requested {
                o.cancel(&task_id)?;
            }
            if let Err(error) = self.run_guard(&run.id, false)?.ensure_current() {
                o.cancel(&task_id)?;
                return Err(error);
            }
            let record = self.engine.store.transact_records::<_, OrchError>(
                self.project.id.as_str(),
                |tx| {
                    Ok(serde_json::from_value::<TaskRecord>(
                        tx.get_record(TASK, &task_id)?
                            .ok_or_else(|| OrchError::UnknownTask(task_id.clone()))?,
                    )?)
                },
            )?;
            if record.state.is_final() && record.execution_owner.is_none() {
                if record.state != TaskState::Done {
                    return Err(OrchError::NotAllowed(
                        record
                            .error
                            .unwrap_or_else(|| "the agent did not complete its step".into()),
                    ));
                }
                return Ok(Output {
                    text: record.result.unwrap_or_default(),
                    files: record.outputs,
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Inner {
    pub(super) fn admit_frozen_automation_run(
        &self,
        admission: FrozenRunAdmission<'_>,
    ) -> Result<Run, OrchError> {
        self.admit_frozen_automation_run_tracked(admission)
            .map(|(run, _)| run)
    }

    pub(super) fn admit_frozen_automation_run_tracked(
        &self,
        admission: FrozenRunAdmission<'_>,
    ) -> Result<(Run, bool), OrchError> {
        let FrozenRunAdmission {
            automation,
            expected,
            origin,
            inputs,
            input_digest,
            prepared,
            due_now,
            authenticated_connection,
        } = admission;
        origin.validate().map_err(OrchError::NotAllowed)?;
        let id = origin.run_id(self.project.id.as_str(), automation)?;
        let mut state = self.state.lock().unwrap();
        let (run, item, board, schedule) =
            self.engine
                .store
                .transact_records(self.project.id.as_str(), |tx| {
                    if let Some(value) = tx.get_record(automation::RUN, &id)? {
                        let prior: Run = serde_json::from_value(value)?;
                        if prior.input_digest != input_digest {
                            return Err(OrchError::NotAllowed(
                                "this trigger identity already names another execution".into(),
                            ));
                        }
                        return Ok::<_, OrchError>((prior, None, None, None));
                    }
                    if matches!(origin, RunOrigin::Manual { .. })
                        && tx.get_record("automation_start_cancelled", &id)?.is_some()
                    {
                        return Err(OrchError::NotAllowed(
                            "this manual start was cancelled; use a new request".into(),
                        ));
                    }
                    let head: automation::Definition = serde_json::from_value(
                        tx.get_record(automation::DEFINITION, automation)?
                            .ok_or_else(unavailable)?,
                    )?;
                    let manual = matches!(origin, RunOrigin::Manual { .. });
                    if manual && head.revision != expected
                        || !manual
                            && ((!head.enabled
                                && !matches!(origin, RunOrigin::ScheduleManual { .. }))
                                || head.active_revision != Some(expected)
                                || head.activation.as_deref() != Some(&prepared.value.id))
                    {
                        return Err(OrchError::NotAllowed(
                            "the automation changed before admission".into(),
                        ));
                    }
                    if !manual {
                        let grant: automations::PreparedActivation = serde_json::from_value(
                            tx.get_record(automations::ACTIVATION, &prepared.value.id)?
                                .ok_or_else(unavailable)?,
                        )?;
                        if grant.value.phase != ActivationPhase::Active {
                            return Err(OrchError::NotAllowed(
                                "the activation was revoked before admission".into(),
                            ));
                        }
                    }
                    if let RunOrigin::Manual { request } = &origin {
                        let pending: automation::ManualStart = serde_json::from_value(
                            tx.get_record(automation::MANUAL_START, automation)?
                                .ok_or_else(|| {
                                    OrchError::NotAllowed(
                                        "prepare this manual start before admission".into(),
                                    )
                                })?,
                        )?;
                        if pending.request != *request
                            || pending.expected != expected
                            || pending.inputs != inputs
                        {
                            return Err(OrchError::NotAllowed(
                                "another manual start is pending; recover it first".into(),
                            ));
                        }
                    }
                    if manual {
                        for (name, spec) in &prepared.observed {
                            let agent: AgentRecord = serde_json::from_value(
                                tx.get_record(AGENT, &format!("{}/{name}", self.project.id))?
                                    .ok_or_else(|| OrchError::UnknownAgent(name.clone()))?,
                            )?;
                            if agent.state == AgentState::Terminated
                                || serde_json::to_value(&agent.spec)? != serde_json::to_value(spec)?
                            {
                                return Err(OrchError::NotAllowed(
                                    "an agent changed before execution admission".into(),
                                ));
                            }
                        }
                    }
                    let revision = automations::revision(tx, automation, expected)?;
                    revision
                        .validate_inputs(&inputs)
                        .map_err(OrchError::NotAllowed)?;
                    match (&revision.draft.trigger, &origin) {
                        (_, RunOrigin::Manual { .. })
                        | (
                            automation::Trigger::Schedule { .. },
                            RunOrigin::Schedule { .. } | RunOrigin::ScheduleManual { .. },
                        )
                        | (automation::Trigger::Webhook, RunOrigin::Webhook { .. }) => {}
                        _ => {
                            return Err(OrchError::NotAllowed(
                                "the trigger does not match this automation".into(),
                            ))
                        }
                    }
                    for input in inputs.values() {
                        if let InputValue::Files { ids } = input {
                            for id in ids {
                                if tx.get_artifact(id)?.is_none() {
                                    return Err(OrchError::NotAllowed(
                                        "an input file is unavailable in this project".into(),
                                    ));
                                }
                            }
                        }
                    }
                    let due_schedule = if let RunOrigin::Schedule { schedule, slot_ms } = &origin {
                        let now = due_now.ok_or_else(|| {
                            OrchError::NotAllowed(
                                "scheduled admission requires the timer occurrence".into(),
                            )
                        })?;
                        let mut record: ScheduleRecord = serde_json::from_value(
                            tx.get_record(SCHEDULE, schedule)?.ok_or_else(unavailable)?,
                        )?;
                        if head.schedule.as_ref() != Some(schedule)
                            || record.target.as_deref() != Some(automation)
                            || record.paused
                            || record.next_ms != Some(*slot_ms)
                            || *slot_ms <= record.target_since_ms
                            || now < *slot_ms
                        {
                            return Err(OrchError::NotAllowed(
                                "the scheduled target or occurrence changed before admission"
                                    .into(),
                            ));
                        }
                        let open = tx
                            .records_matching_states(
                                automation::RUN,
                                "origin.schedule",
                                schedule,
                                "phase",
                                &["pending", "running", "waiting_approval"],
                                MAX_OPEN_RUNS + 1,
                            )?
                            .len();
                        if open >= MAX_OPEN_RUNS || !record.overlap && open > 0 {
                            return Err(OrchError::NotAllowed(
                                "the earlier scheduled execution is still open".into(),
                            ));
                        }
                        record.runs = record.runs.saturating_add(1);
                        record.last_ms = Some(now);
                        record.last_task = Some(id.clone());
                        record.advance(*slot_ms, now);
                        record.version = record.version.checked_add(1).ok_or_else(|| {
                            OrchError::NotAllowed("the schedule version limit was reached".into())
                        })?;
                        tx.put_record(SCHEDULE, schedule, &serde_json::to_value(&record)?)?;
                        Some(record)
                    } else if let RunOrigin::ScheduleManual { schedule, .. } = &origin {
                        let mut record: ScheduleRecord = serde_json::from_value(
                            tx.get_record(SCHEDULE, schedule)?.ok_or_else(unavailable)?,
                        )?;
                        if head.schedule.as_ref() != Some(schedule)
                            || record.target.as_deref() != Some(automation)
                        {
                            return Err(OrchError::NotAllowed(
                                "the scheduled target changed before manual admission".into(),
                            ));
                        }
                        let open = tx
                            .records_matching_states(
                                automation::RUN,
                                "automation",
                                automation,
                                "phase",
                                &["pending", "running", "waiting_approval"],
                                MAX_OPEN_RUNS + 1,
                            )?
                            .len();
                        if open >= MAX_OPEN_RUNS || !record.overlap && open > 0 {
                            return Err(OrchError::NotAllowed(
                                "the earlier execution is still open".into(),
                            ));
                        }
                        record.runs = record.runs.saturating_add(1);
                        record.last_ms = Some(now_ms());
                        record.last_task = Some(id.clone());
                        record.version = record.version.checked_add(1).ok_or_else(|| {
                            OrchError::NotAllowed("the schedule version limit was reached".into())
                        })?;
                        let RunOrigin::ScheduleManual { request, .. } = &origin else {
                            return Err(OrchError::NotAllowed("the manual trigger changed".into()));
                        };
                        let key = crate::schedule::manual_receipt_id(
                            self.project.id.as_str(),
                            schedule,
                            request,
                        )?;
                        tx.insert_record(
                            crate::schedule::MANUAL_RECEIPT,
                            &key,
                            &serde_json::to_value(crate::ScheduleRunReceipt {
                                schedule: schedule.clone(),
                                request: request.clone(),
                                execution: id.clone(),
                                kind: crate::ScheduleExecutionKind::Automation,
                                created_ms: now_ms(),
                            })?,
                        )?;
                        tx.put_record(SCHEDULE, schedule, &serde_json::to_value(&record)?)?;
                        Some(record)
                    } else {
                        None
                    };
                    if let RunOrigin::Webhook {
                        endpoint,
                        account,
                        source,
                        ..
                    } = &origin
                    {
                        let record = super::webhooks::endpoint(tx, endpoint)?;
                        let active = record.active.as_ref().ok_or_else(|| {
                            OrchError::NotAllowed("the webhook has no active connection".into())
                        })?;
                        let grant = prepared.value.webhook.as_ref().ok_or_else(|| {
                            OrchError::NotAllowed(
                                "the activation has no verified webhook connection".into(),
                            )
                        })?;
                        if !record.enabled
                            || record.automation != automation
                            || record.project != self.project.id.as_str()
                            || &record.account != account
                            || source != "webhook"
                            || authenticated_connection.is_none_or(|(connection, at)| {
                                connection != active.id || at <= record.active_since_ms
                            })
                            || record.active_snapshot().as_ref() != Some(grant)
                        {
                            return Err(OrchError::NotAllowed(
                                "the verified webhook connection changed before admission".into(),
                            ));
                        }
                        let pending = tx
                            .records_matching_states(
                                automation::RUN,
                                "origin.endpoint",
                                endpoint,
                                "phase",
                                &["pending", "running", "waiting_approval"],
                                21,
                            )?
                            .len();
                        if pending >= 20 {
                            return Err(OrchError::NotAllowed(
                                "the webhook pending-run limit was reached".into(),
                            ));
                        }
                        let window = now_ms() / 3_600_000 * 3_600_000;
                        if !tx.reserve_quota(
                            &format!("webhook:{endpoint}"),
                            window,
                            grant.max_per_hour,
                        )? || !tx.reserve_quota("webhook:all", window, 1000)?
                        {
                            return Err(OrchError::NotAllowed(
                                "the webhook admission limit was reached".into(),
                            ));
                        }
                    }
                    let mut data = work::WorkData::load(tx)?;
                    let board =
                        data.ensure_board(tx, self.project.id.as_str(), PERSON, FIRST_GOAL, None)?;
                    let mut item = WorkItem::new(
                        self.project.id.as_str(),
                        &board.id,
                        PERSON,
                        &revision.draft.name,
                    );
                    let mut root = SessionRecord::empty(
                        SessionId::new(),
                        self.project.id.clone(),
                        CarryPolicy::default(),
                        None,
                    );
                    root.title = revision.draft.name.clone();
                    root.work_item = Some(item.id.clone());
                    tx.create_session(root.id.as_str(), &serde_json::to_value(&root)?)?;
                    item.conversation = Some(root.id.to_string());
                    item.run = Some(id.clone());
                    item.state = WorkState::Doing;
                    let mut steps = Vec::new();
                    for step in &revision.draft.steps {
                        let scope =
                            Project::new(&step.name, "").with_parent(self.project.id.clone());
                        tx.create_execution_project(&scope)?;
                        let model = prepared
                            .value
                            .selections
                            .get(&step.id)
                            .map(|selected| selected.model.clone());
                        let mut session = SessionRecord::empty(
                            SessionId::new(),
                            scope.id.clone(),
                            CarryPolicy::default(),
                            model,
                        );
                        session.title = step.name.clone();
                        session.work_item = Some(item.id.clone());
                        session.execution_managed = true;
                        tx.create_execution_session(
                            session.id.as_str(),
                            &serde_json::to_value(&session)?,
                        )?;
                        steps.push(StepExecution {
                            id: step.id.clone(),
                            scope: scope.id.to_string(),
                            conversation: session.id.to_string(),
                            phase: StepPhase::Pending,
                            receipt: None,
                            output: None,
                            question: None,
                            error: None,
                        });
                    }
                    let now = now_ms();
                    let run = Run {
                        id: id.clone(),
                        project: self.project.id.to_string(),
                        automation: automation.into(),
                        revision,
                        activation: (!manual).then(|| prepared.value.id.clone()),
                        selections: prepared.value.selections.clone(),
                        agents: prepared.value.agents.clone(),
                        descriptors: prepared.value.descriptors.clone(),
                        origin: origin.clone(),
                        inputs: inputs.clone(),
                        input_digest: input_digest.clone(),
                        phase: RunPhase::Pending,
                        work: item.id.clone(),
                        conversation: root.id.to_string(),
                        steps,
                        questions: Default::default(),
                        output: Output::default(),
                        error: None,
                        cancel_requested: false,
                        created_ms: now,
                        updated_ms: now,
                        owner: None,
                    };
                    tx.insert_record(automation::RUN, &id, &serde_json::to_value(&run)?)?;
                    tx.insert_record(WORK, &item.id, &serde_json::to_value(&item)?)?;
                    Ok((run, Some(item), Some(board), due_schedule))
                })?;
        let created = item.is_some();
        if let Some(schedule) = schedule {
            state.schedules.insert(schedule.id.clone(), schedule);
        }
        if let Some(board) = board {
            state.boards.insert(board.id.clone(), board);
        }
        if let Some(item) = item {
            state.work.insert(item.id.clone(), item.clone());
            self.emit(EventKind::Work {
                item,
                by: PERSON.into(),
            });
        }
        self.emit(EventKind::Automation {
            automation: automation.into(),
            run: Some(run.id.clone()),
        });
        Ok((run, created))
    }
}

#[cfg(test)]
mod approval_tests {
    use super::*;
    struct Send;
    #[async_trait::async_trait]
    impl ark_tools::Adapter for Send {
        fn name(&self) -> &str {
            "send"
        }
        fn actions(&self) -> Vec<ark_core::ActionDescriptor> {
            vec![ark_core::ActionDescriptor::new(
                "send.message",
                "send a message",
                ark_core::RiskTier::Mutate,
            )
            .auto_eligible()]
        }
        async fn run(
            &self,
            _: &str,
            _: serde_json::Value,
            _: &ark_tools::RunCtx<'_>,
        ) -> ark_tools::Result<ark_tools::ActionOutput> {
            unreachable!()
        }
    }

    async fn fixture() -> (tempfile::TempDir, Orchestrator, Run) {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(ark_model::MockProvider::new("test", vec![]));
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(provider.clone())
            .tool(Arc::new(Send))
            .build()
            .unwrap();
        let project = ark.create_project("approval concurrency", "").unwrap();
        let o = Orchestrator::builder(&ark, &project)
            .models(Arc::new(
                crate::Models::builder()
                    .default_model("test")
                    .provider("test", provider)
                    .build(),
            ))
            .build()
            .unwrap();
        let draft = automation::Draft {
            name: "Review effects".into(),
            trigger: automation::Trigger::Manual,
            inputs: vec![],
            authority: automation::Authority {
                actions: vec![automation::AllowedAction {
                    action: "send.message".into(),
                    inputs: std::collections::BTreeMap::from([(
                        "message".into(),
                        automation::Bound::Free,
                    )]),
                    unattended: false,
                    per_run: 10,
                    quota: automation::Quota {
                        count: 10,
                        window_ms: 3_600_000,
                    },
                }],
                ..Default::default()
            },
            steps: vec![automation::Step {
                id: "result".into(),
                name: "Result".into(),
                action: automation::StepAction::Result { inputs: vec![] },
            }],
        };
        let saved = o.save_automation("create", None, None, draft).unwrap();
        let mut run = o
            .admit_confirmed_test_run(
                &saved.automation,
                1,
                RunOrigin::Manual {
                    request: "one".into(),
                },
                Default::default(),
            )
            .await
            .unwrap();
        run.phase = RunPhase::Running;
        run.steps[0].phase = automation::StepPhase::Running;
        run.owner = Some(RunOwner {
            token: o.inner.execution_token.clone(),
            pid: std::process::id(),
            pid_start: process::own_start(),
        });
        o.inner
            .engine
            .store
            .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
                tx.put_record(automation::RUN, &run.id, &serde_json::to_value(&run)?)
                    .map_err(Into::into)
            })
            .unwrap();
        (dir, o, run)
    }

    fn request(o: &Orchestrator, run: &Run, value: &str) -> ApprovalRequest {
        let descriptor = o.inner.engine.tools().descriptor("send.message").unwrap();
        let mut call = ark_core::Invocation::new(
            "send.message",
            descriptor.risk,
            serde_json::json!({"message": value}),
        );
        call.status = ark_core::InvocationStatus::PendingApproval;
        call.work = Some(format!("{}/{}", run.id, run.steps[0].id));
        call.descriptor = Some(
            blake3::hash(&serde_json::to_vec(&descriptor).unwrap())
                .to_hex()
                .to_string(),
        );
        let scope = ProjectId(run.steps[0].scope.clone());
        o.inner
            .engine
            .store
            .create_invocation(&scope, &call)
            .unwrap();
        ApprovalRequest {
            invocation: Some(call.id),
            project: Some(scope),
            action: "send.message".into(),
            summary: value.into(),
            risk: ark_core::RiskTier::Mutate,
            input: serde_json::json!({"message": value}),
            class: "send.message".into(),
            preview: value.into(),
            agent: None,
            task: None,
            ask_each_time: true,
            cut_short: false,
        }
    }

    #[tokio::test]
    async fn concurrent_questions_keep_distinct_decisions_and_wait_for_all_answers() {
        let (_dir, o, run) = fixture().await;
        let approver = Arc::new(RunApprover {
            inner: o.inner.clone(),
            run: run.id.clone(),
            index: 0,
            task: None,
            guard: o.inner.run_guard(&run.id, true).unwrap(),
        });
        let first_request = request(&o, &run, "first");
        let second_request = request(&o, &run, "second");
        let first = approver.clone();
        let first = tokio::spawn(async move { first.decide(&first_request).await });
        let second = approver.clone();
        let second = tokio::spawn(async move { second.decide(&second_request).await });
        let pending = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let observed = o.automation_run(&run.id).unwrap();
                if observed.questions.len() == 2 {
                    break observed;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let question = pending
            .questions
            .values()
            .find(|question| question.detail == "second")
            .unwrap();
        o.answer_automation_question(
            &run.id,
            &question.id,
            &question.digest,
            ApprovalDecision::Deny,
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!first.is_finished());
        assert!(!second.is_finished());
        assert_eq!(
            o.automation_run(&run.id).unwrap().phase,
            RunPhase::WaitingApproval
        );
        let question = pending
            .questions
            .values()
            .find(|question| question.detail == "first")
            .unwrap();
        assert!(o
            .answer_automation_question(
                &run.id,
                &question.id,
                "wrong digest",
                ApprovalDecision::AllowOnce
            )
            .is_err());
        o.answer_automation_question(
            &run.id,
            &question.id,
            &question.digest,
            ApprovalDecision::AllowOnce,
        )
        .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), first)
                .await
                .unwrap()
                .unwrap(),
            ApprovalDecision::AllowOnce
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), second)
                .await
                .unwrap()
                .unwrap(),
            ApprovalDecision::Deny
        );
        let observed = o.automation_run(&run.id).unwrap();
        assert_eq!(observed.phase, RunPhase::Running);
        assert_eq!(observed.questions.len(), 2);
    }
}
