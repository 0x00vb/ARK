use super::*;
use crate::automation::{self, ActivationPhase, Definition, Run, RunOrigin};

impl Inner {
    pub(super) fn dispatch_due_automation(
        &self,
        id: &str,
        now: u64,
    ) -> Result<Option<Run>, OrchError> {
        let mut state = self.state.lock().unwrap();
        let (schedule, prepared) =
            self.engine
                .store
                .transact_records(self.project.id.as_str(), |tx| {
                    let mut record: ScheduleRecord =
                        serde_json::from_value(tx.get_record(SCHEDULE, id)?.ok_or_else(|| {
                            OrchError::NotAllowed("the schedule is unavailable".into())
                        })?)?;
                    let automation = record.target.as_deref().ok_or_else(|| {
                        OrchError::NotAllowed("this schedule does not target an automation".into())
                    })?;
                    let head: Definition = serde_json::from_value(
                        tx.get_record(automation::DEFINITION, automation)?
                            .ok_or_else(|| {
                                OrchError::NotAllowed("the automation is unavailable".into())
                            })?,
                    )?;
                    if !record.due(now) || !head.enabled {
                        return Ok::<_, OrchError>((record, None));
                    }
                    let slot = record.next_ms.ok_or_else(|| {
                        OrchError::NotAllowed("the schedule has no due occurrence".into())
                    })?;
                    let active = tx
                        .records_matching_states(
                            automation::RUN,
                            "origin.schedule",
                            id,
                            "phase",
                            &["pending", "running", "waiting_approval"],
                            MAX_OPEN_RUNS + 1,
                        )?
                        .len();
                    let missed = now.saturating_sub(slot) > crate::schedule::LATE_MS
                        && record.if_missed == IfMissed::Skip;
                    let replaced = slot <= record.target_since_ms;
                    if replaced
                        || missed
                        || active >= MAX_OPEN_RUNS
                        || !record.overlap && active > 0
                    {
                        if missed && !replaced {
                            record.missed = record.missed.saturating_add(1);
                        } else {
                            record.skipped = record.skipped.saturating_add(1);
                        }
                        record.advance(slot, now.max(record.target_since_ms));
                        record.version = record.version.checked_add(1).ok_or_else(|| {
                            OrchError::NotAllowed("the schedule version limit was reached".into())
                        })?;
                        tx.put_record(SCHEDULE, id, &serde_json::to_value(&record)?)?;
                        return Ok((record, None));
                    }
                    let prepared: automations::PreparedActivation = serde_json::from_value(
                        tx.get_record(
                            automations::ACTIVATION,
                            head.activation.as_deref().ok_or_else(|| {
                                OrchError::NotAllowed("the automation has no activation".into())
                            })?,
                        )?
                        .ok_or_else(|| {
                            OrchError::NotAllowed("the activation is unavailable".into())
                        })?,
                    )?;
                    if prepared.value.phase != ActivationPhase::Active
                        || Some(prepared.value.revision) != head.active_revision
                    {
                        return Err(OrchError::NotAllowed(
                            "the automation activation changed".into(),
                        ));
                    }
                    Ok((record, Some(prepared)))
                })?;
        state
            .schedules
            .insert(schedule.id.clone(), schedule.clone());
        drop(state);
        let Some(prepared) = prepared else {
            return Ok(None);
        };
        let automation = schedule
            .target
            .as_deref()
            .ok_or_else(|| OrchError::NotAllowed("the scheduled automation changed".into()))?;
        let origin = RunOrigin::Schedule {
            schedule: id.into(),
            slot_ms: schedule
                .next_ms
                .ok_or_else(|| OrchError::NotAllowed("the schedule occurrence changed".into()))?,
        };
        let inputs = Default::default();
        let digest = origin.input_digest(automation, prepared.value.revision, &inputs)?;
        self.admit_frozen_automation_run(runs::FrozenRunAdmission {
            automation,
            expected: prepared.value.revision,
            origin,
            inputs,
            input_digest: digest,
            prepared,
            due_now: Some(now),
            authenticated_connection: None,
        })
        .map(Some)
    }
}

enum ManualAdmission {
    Existing(Box<Run>),
    Prepared {
        automation: String,
        revision: u64,
        prepared: Box<automations::PreparedActivation>,
    },
}

impl Inner {
    pub(super) fn admit_manual_schedule_run(
        &self,
        id: &str,
        request: &str,
    ) -> Result<Run, OrchError> {
        let admission = self
            .engine
            .store
            .transact_records(self.project.id.as_str(), |tx| {
                let record: ScheduleRecord =
                    serde_json::from_value(tx.get_record(SCHEDULE, id)?.ok_or_else(|| {
                        OrchError::NotAllowed("the schedule is unavailable".into())
                    })?)?;
                let automation = record
                    .target
                    .ok_or_else(|| OrchError::NotAllowed("the scheduled target changed".into()))?;
                let origin = RunOrigin::ScheduleManual {
                    schedule: id.into(),
                    request: request.into(),
                };
                origin.validate().map_err(OrchError::NotAllowed)?;
                let run_id = origin.run_id(self.project.id.as_str(), &automation)?;
                if let Some(value) = tx.get_record(automation::RUN, &run_id)? {
                    let run: Run = serde_json::from_value(value)?;
                    if run.origin != origin
                        || run.automation != automation
                        || run.project != self.project.id.as_str()
                    {
                        return Err(OrchError::NotAllowed(
                            "the trigger identity names another execution".into(),
                        ));
                    }
                    return Ok::<_, OrchError>(ManualAdmission::Existing(Box::new(run)));
                }
                let head: Definition = serde_json::from_value(
                    tx.get_record(automation::DEFINITION, &automation)?
                        .ok_or_else(|| {
                            OrchError::NotAllowed("the automation is unavailable".into())
                        })?,
                )?;
                let revision = head.active_revision.ok_or_else(|| {
                    OrchError::NotAllowed("the automation has no active revision".into())
                })?;
                let prepared: automations::PreparedActivation = serde_json::from_value(
                    tx.get_record(
                        automations::ACTIVATION,
                        head.activation.as_deref().ok_or_else(|| {
                            OrchError::NotAllowed("the automation has no activation".into())
                        })?,
                    )?
                    .ok_or_else(|| OrchError::NotAllowed("the activation is unavailable".into()))?,
                )?;
                if prepared.value.phase != ActivationPhase::Active {
                    return Err(OrchError::NotAllowed("the activation was revoked".into()));
                }
                Ok(ManualAdmission::Prepared {
                    automation,
                    revision,
                    prepared: Box::new(prepared),
                })
            })?;
        let (automation, revision, prepared) = match admission {
            ManualAdmission::Existing(run) => return Ok(*run),
            ManualAdmission::Prepared {
                automation,
                revision,
                prepared,
            } => (automation, revision, prepared),
        };
        let origin = RunOrigin::ScheduleManual {
            schedule: id.into(),
            request: request.into(),
        };
        let inputs = Default::default();
        let input_digest = origin.input_digest(&automation, revision, &inputs)?;
        self.admit_frozen_automation_run(runs::FrozenRunAdmission {
            automation: &automation,
            expected: revision,
            origin,
            inputs,
            input_digest,
            prepared: *prepared,
            due_now: None,
            authenticated_connection: None,
        })
    }
}

impl Orchestrator {
    pub fn calendar_snapshot(
        &self,
        from: u64,
        to: u64,
    ) -> Result<crate::schedule::CalendarSnapshot, OrchError> {
        if from >= to || to - from > 62 * 86_400_000 {
            return Err(OrchError::NotAllowed(
                "the calendar window must be at most 62 days".into(),
            ));
        }
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let schedules = tx
                    .records_page(SCHEDULE, "created_ms", None, 1001)?
                    .into_iter()
                    .map(serde_json::from_value::<ScheduleRecord>)
                    .collect::<Result<Vec<_>, _>>()?;
                let tasks = tx
                    .records_overlapping(
                        TASK,
                        "created_ms",
                        "finished_ms",
                        from..to,
                        Some("schedule"),
                        1001,
                    )?
                    .into_iter()
                    .map(serde_json::from_value::<TaskRecord>)
                    .collect::<Result<Vec<_>, _>>()?;
                let mut runs = tx
                    .records_overlapping(
                        automation::RUN,
                        "created_ms",
                        "updated_ms",
                        from..to,
                        None,
                        1001,
                    )?
                    .into_iter()
                    .map(serde_json::from_value::<Run>)
                    .collect::<Result<Vec<_>, _>>()?;
                for value in tx.records_states_after(
                    automation::RUN,
                    "phase",
                    &["pending", "running", "waiting_approval"],
                    None,
                    1001,
                )? {
                    let run: Run = serde_json::from_value(value)?;
                    if run.created_ms < to && !runs.iter().any(|candidate| candidate.id == run.id) {
                        runs.push(run);
                    }
                }
                if schedules.len() > 1000 || tasks.len() + runs.len() > 1000 {
                    return Err(OrchError::NotAllowed(
                        "the calendar has too many executions; choose a shorter period".into(),
                    ));
                }
                Ok(crate::schedule::CalendarSnapshot {
                    schedules,
                    tasks,
                    runs,
                })
            })
    }
}

pub(super) fn settle(
    tx: &ark_store::RecordTransaction<'_>,
    run: &Run,
) -> Result<Option<ScheduleRecord>, OrchError> {
    let id = match &run.origin {
        RunOrigin::Schedule { schedule, .. } | RunOrigin::ScheduleManual { schedule, .. } => {
            schedule
        }
        _ => return Ok(None),
    };
    let Some(value) = tx.get_record(SCHEDULE, id)? else {
        return Ok(None);
    };
    let mut schedule: ScheduleRecord = serde_json::from_value(value)?;
    if schedule.target.as_deref() != Some(&run.automation)
        || schedule.last_task.as_deref() != Some(&run.id)
        || schedule.counted.as_deref() == Some(&run.id)
    {
        return Ok(None);
    }
    schedule.counted = Some(run.id.clone());
    match run.phase {
        automation::RunPhase::Failed => schedule.failures = schedule.failures.saturating_add(1),
        automation::RunPhase::Completed => schedule.failures = 0,
        _ => {}
    }
    if schedule.failures >= MAX_FAILURES && !schedule.paused {
        schedule.paused = true;
        schedule.next_ms = None;
        let mut head: Definition = serde_json::from_value(
            tx.get_record(automation::DEFINITION, &run.automation)?
                .ok_or_else(|| OrchError::NotAllowed("the automation is unavailable".into()))?,
        )?;
        head.enabled = false;
        head.updated_ms = now_ms();
        tx.put_record(
            automation::DEFINITION,
            &head.id,
            &serde_json::to_value(&head)?,
        )?;
    }
    schedule.version = schedule
        .version
        .checked_add(1)
        .ok_or_else(|| OrchError::NotAllowed("the schedule version limit was reached".into()))?;
    tx.put_record(SCHEDULE, id, &serde_json::to_value(&schedule)?)?;
    Ok(Some(schedule))
}
