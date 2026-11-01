use super::*;
use crate::automation::{
    self, Binding, LegacyAdoption, LegacyPolicies, LegacyProjection, Port, Step, StepAction,
    Trigger,
};

pub(super) fn config_digest(record: &ScheduleRecord) -> Result<String, serde_json::Error> {
    Ok(format!(
        "v1:{}",
        blake3::hash(&serde_json::to_vec(&(
            &record.agent,
            &record.task,
            &record.when,
            record.if_missed,
            record.overlap,
            record.retries,
            record.own_agent,
            record.paused
        ))?)
        .to_hex()
    ))
}

pub(super) fn validate_source(
    tx: &ark_store::RecordTransaction<'_>,
    revision: &automation::Revision,
) -> Result<(), OrchError> {
    let Some(policies) = &revision.legacy else {
        return Ok(());
    };
    let record: ScheduleRecord = serde_json::from_value(
        tx.get_record(SCHEDULE, &policies.schedule)?
            .ok_or_else(|| OrchError::NotAllowed("the original schedule is unavailable".into()))?,
    )?;
    if record.target.as_deref() == Some(&revision.automation) {
        return Ok(());
    }
    if record.target.is_some() || config_digest(&record)? != policies.source_digest {
        return Err(OrchError::NotAllowed(
            "the original schedule changed; review it before activation".into(),
        ));
    }
    Ok(())
}

impl Orchestrator {
    fn legacy_schedule(&self, id: &str) -> Result<ScheduleRecord, OrchError> {
        self.inner.engine.store.transact_records(self.project().id.as_str(), |tx| {
            let record: ScheduleRecord = serde_json::from_value(tx.get_record(SCHEDULE,id)?.ok_or_else(|| OrchError::NotAllowed("the schedule is unavailable".into()))?)?;
            if record.target.is_some() { return Err(OrchError::NotAllowed("this schedule already runs an automation".into())); }
            if record.task.item.is_some() { return Err(OrchError::NotAllowed("this schedule shares a board item; use its existing controls to preserve that grouping".into())); }
            if let Some(chat) = &record.task.report_to {
                if tx.get_session(chat)?.is_none() { return Err(OrchError::NotAllowed("the result conversation is unavailable in this project".into())); }
            }
            Ok(record)
        })
    }

    pub(super) fn legacy_policies(
        &self,
        adoption: &LegacyAdoption,
    ) -> Result<(ScheduleRecord, LegacyPolicies), OrchError> {
        let schedule = self.legacy_schedule(&adoption.schedule)?;
        let config = config_digest(&schedule)?;
        if adoption
            .config
            .as_ref()
            .is_some_and(|digest| digest != &config)
            || schedule.version != adoption.expected && adoption.config.as_ref() != Some(&config)
        {
            return Err(OrchError::NotAllowed(
                "the schedule changed; review it before saving".into(),
            ));
        }
        let book = RuleBook::new(&self.inner.engine.store);
        let mut rules = Vec::new();
        for binding in &schedule.task.rules {
            let set = book
                .get(&binding.ruleset, binding.version)?
                .ok_or_else(|| {
                    OrchError::NotAllowed(format!(
                        "the rule set {} is unavailable",
                        binding.ruleset
                    ))
                })?;
            rules.push(RuleBinding {
                ruleset: set.name,
                version: Some(set.version),
                scope: binding.scope.clone(),
            });
        }
        let policies = LegacyPolicies {
            schedule: schedule.id.clone(),
            source_version: schedule.version,
            source_digest: config,
            step: "legacy-work".into(),
            after: schedule.task.after.clone(),
            budget: schedule.task.budget.clone(),
            rules,
            work: schedule.task.work.clone(),
            report_to: schedule.task.report_to.clone(),
            max_attempts: schedule.retries.saturating_add(1),
            retry_delay_ms: crate::schedule::RETRY_DELAY_MS,
        };
        Ok((schedule, policies))
    }

    pub fn legacy_automation(&self, id: &str) -> Result<LegacyProjection, OrchError> {
        let schedule = self.legacy_schedule(id)?;
        let (_, policies) = self.legacy_policies(&LegacyAdoption {
            schedule: id.into(),
            expected: schedule.version,
            config: None,
        })?;
        let mut inputs = Vec::new();
        if !schedule.task.context.is_empty() {
            inputs.push(Binding::Literal {
                text: schedule.task.context.clone(),
            });
        }
        let mut references = schedule.task.artifacts.clone();
        for reference in ark_runtime::session::artifact_references(&format!(
            "{}\n{}",
            schedule.task.objective, schedule.task.context
        ))? {
            if !references.contains(&reference) {
                references.push(reference);
            }
        }
        for reference in references {
            let artifact = self
                .inner
                .engine
                .store
                .resolve_artifact(&self.project().id, &reference)?
                .ok_or_else(|| {
                    OrchError::NotAllowed(format!(
                        "the input file {reference} is unavailable in this project"
                    ))
                })?;
            if !inputs
                .iter()
                .any(|binding| matches!(binding,Binding::Artifact{id} if id==artifact.id.as_str()))
            {
                inputs.push(Binding::Artifact {
                    id: artifact.id.to_string(),
                });
            }
        }
        let draft = automation::Draft {
            name: clip(&schedule.task.objective, 200),
            trigger: Trigger::Schedule {
                when: schedule.when.clone(),
            },
            inputs: vec![],
            authority: Default::default(),
            steps: vec![
                Step {
                    id: policies.step.clone(),
                    name: "Work".into(),
                    action: StepAction::Agent {
                        target: WorkTarget::Agent {
                            name: schedule.agent.clone(),
                        },
                        prompt: schedule.task.objective.clone(),
                        inputs,
                    },
                },
                Step {
                    id: "legacy-result".into(),
                    name: "Result".into(),
                    action: StepAction::Result {
                        inputs: vec![
                            Binding::Step {
                                step: policies.step.clone(),
                                port: Port::Text,
                            },
                            Binding::Step {
                                step: policies.step.clone(),
                                port: Port::Files,
                            },
                        ],
                    },
                },
            ],
        }
        .validated()
        .map_err(OrchError::NotAllowed)?;
        Ok(LegacyProjection {
            schedule: id.into(),
            version: schedule.version,
            draft,
            policies,
        })
    }
}
