use super::*;
use crate::automation::{self, Definition, Draft, Entry, Layout, Page, Revision, SaveReceipt};
use ark_store::RecordTransaction;
use serde::{Deserialize, Serialize};

const SAVE: &str = "automation_save_receipt";

#[derive(Serialize, Deserialize)]
struct Saved {
    digest: String,
    receipt: Option<SaveReceipt>,
}

fn webhook_availability(
    tx: &RecordTransaction<'_>,
    head: &Definition,
) -> Result<Option<automation::WebhookAvailability>, OrchError> {
    if !head.enabled {
        return Ok(None);
    }
    let Some(id) = &head.webhook else {
        return Ok(None);
    };
    let endpoint = super::webhooks::endpoint(tx, id)?;
    Ok(if endpoint.enabled {
        endpoint.active.map(|connection| connection.availability)
    } else {
        None
    })
}

pub(super) fn revision(
    tx: &RecordTransaction<'_>,
    id: &str,
    number: u64,
) -> Result<Revision, OrchError> {
    let value = tx
        .get_record(automation::REVISION, &format!("{id}/{number}"))?
        .ok_or_else(|| OrchError::NotAllowed("the automation revision is unavailable".into()))?;
    let revision: Revision = serde_json::from_value(value)?;
    if revision.automation != id
        || revision.number != number
        || revision.computed_digest()? != revision.digest
    {
        return Err(OrchError::NotAllowed(
            "the automation revision does not match its identity".into(),
        ));
    }
    Ok(revision)
}

fn save_digest(
    id: Option<&str>,
    expected: Option<u64>,
    draft: &Draft,
    adoption: Option<&automation::LegacyAdoption>,
) -> Result<String, serde_json::Error> {
    Ok(if let Some(adoption) = adoption {
        format!(
            "v2:{}",
            blake3::hash(&serde_json::to_vec(&(id, expected, draft, adoption))?).to_hex()
        )
    } else {
        format!(
            "v1:{}",
            blake3::hash(&serde_json::to_vec(&(id, expected, draft))?).to_hex()
        )
    })
}

fn draft_issues(
    tx: &ark_store::RecordTransaction<'_>,
    project: &str,
    draft: &Draft,
) -> Result<Vec<automation::ValidationIssue>, OrchError> {
    let mut issues = draft.validation_issues();
    for step in &draft.steps {
        if let automation::StepAction::Agent {
            target: WorkTarget::Agent { name },
            ..
        } = &step.action
        {
            let agent: Option<AgentRecord> = tx
                .get_record(AGENT, &format!("{}/{name}", project))?
                .map(serde_json::from_value)
                .transpose()?;
            if agent
                .as_ref()
                .is_none_or(|agent| agent.state == AgentState::Terminated)
            {
                issues.push(automation::ValidationIssue {
                    step: Some(step.id.clone()),
                    field: "Responsible".into(),
                    message: "Choose an available agent".into(),
                });
            }
        }
        for binding in step.action.inputs() {
            if let automation::Binding::Artifact { id } = binding {
                if tx.get_artifact(id)?.is_none() {
                    issues.push(automation::ValidationIssue {
                        step: Some(step.id.clone()),
                        field: "Inputs".into(),
                        message: "Choose an available project file".into(),
                    });
                    break;
                }
            }
        }
    }
    Ok(issues)
}

impl Orchestrator {
    pub fn validate_automation_draft(
        &self,
        draft: &Draft,
    ) -> Result<Vec<automation::ValidationIssue>, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                draft_issues(tx, self.project().id.as_str(), draft)
            })
    }

    pub fn preview_webhook_inputs(
        &self,
        id: &str,
        expected: u64,
        inputs: std::collections::BTreeMap<String, automation::InputValue>,
    ) -> Result<automation::InputPreview, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let head: Definition = serde_json::from_value(
                    tx.get_record(automation::DEFINITION, id)?.ok_or_else(|| {
                        OrchError::NotAllowed("the automation is unavailable".into())
                    })?,
                )?;
                if head.revision != expected {
                    return Err(OrchError::NotAllowed(
                        "the automation changed; review its current revision".into(),
                    ));
                }
                let revision = revision(tx, id, expected)?;
                if !matches!(revision.draft.trigger, automation::Trigger::Webhook) {
                    return Err(OrchError::NotAllowed(
                        "input preview needs a webhook automation".into(),
                    ));
                }
                if let Some(issue) =
                    draft_issues(tx, self.project().id.as_str(), &revision.draft)?.first()
                {
                    return Err(OrchError::NotAllowed(issue.message.clone()));
                }
                revision
                    .validate_inputs(&inputs)
                    .map_err(OrchError::NotAllowed)?;
                for value in inputs.values() {
                    if let automation::InputValue::Files { ids } = value {
                        for id in ids {
                            if tx.get_artifact(id)?.is_none() {
                                return Err(OrchError::NotAllowed(
                                    "an input file is unavailable in this project".into(),
                                ));
                            }
                        }
                    }
                }
                let bindings = revision
                    .draft
                    .steps
                    .iter()
                    .flat_map(|step| {
                        step.action.inputs().iter().filter_map(|binding| {
                            if let automation::Binding::Input { name, port } = binding {
                                Some(automation::InputPreviewBinding {
                                    step: step.id.clone(),
                                    input: name.clone(),
                                    port: *port,
                                })
                            } else {
                                None
                            }
                        })
                    })
                    .collect();
                Ok(automation::InputPreview {
                    revision: expected,
                    inputs,
                    bindings,
                })
            })
    }

    pub fn save_automation(
        &self,
        request: &str,
        id: Option<&str>,
        expected: Option<u64>,
        draft: Draft,
    ) -> Result<SaveReceipt, OrchError> {
        self.save_automation_source(request, id, expected, draft, None)
    }

    pub fn save_automation_source(
        &self,
        request: &str,
        id: Option<&str>,
        expected: Option<u64>,
        draft: Draft,
        adoption: Option<automation::LegacyAdoption>,
    ) -> Result<SaveReceipt, OrchError> {
        if request.is_empty() || request.len() > 256 {
            return Err(OrchError::NotAllowed(
                "saving needs a bounded request key".into(),
            ));
        }
        let draft = draft.storage_validated().map_err(OrchError::NotAllowed)?;
        let project = self.project().id.as_str();
        let digest = save_digest(id, expected, &draft, adoption.as_ref())?;
        let key = format!(
            "as-{}",
            blake3::hash(&serde_json::to_vec(&(project, request))?).to_hex()
        );
        if let Some(value) = self.inner.engine.store.get_record(SAVE, &key)? {
            let saved: Saved = serde_json::from_value(value)?;
            if saved.digest != digest {
                return Err(OrchError::NotAllowed(
                    "this save request already names another edit".into(),
                ));
            }
            return saved
                .receipt
                .ok_or_else(|| OrchError::NotAllowed("this save request was cancelled".into()));
        }
        let source = adoption
            .as_ref()
            .map(|adoption| self.legacy_policies(adoption))
            .transpose()?;
        let new_id = format!(
            "a-{}",
            blake3::hash(&serde_json::to_vec(&(project, request))?).to_hex()
        );
        let state = self.inner.state.lock().unwrap();
        let receipt = self.inner.engine.store.transact_records(project, |tx| {
            if let Some(value) = tx.get_record(SAVE, &key)? {
                let saved: Saved = serde_json::from_value(value)?;
                if saved.digest != digest {
                    return Err(OrchError::NotAllowed(
                        "this save request already names another edit".into(),
                    ));
                }
                return saved.receipt.ok_or_else(|| {
                    OrchError::NotAllowed("this save request was cancelled".into())
                });
            }
            let now = now_ms();
            let mut head = match id {
                Some(id) => {
                    let head: Definition = serde_json::from_value(
                        tx.get_record(automation::DEFINITION, id)?.ok_or_else(|| {
                            OrchError::NotAllowed("the automation is unavailable".into())
                        })?,
                    )?;
                    if head.project != project || head.id != id || Some(head.revision) != expected {
                        return Err(OrchError::NotAllowed(
                            "the automation changed; reload before saving".into(),
                        ));
                    }
                    head
                }
                None if expected.is_none() => Definition {
                    id: new_id.clone(),
                    project: project.into(),
                    revision: 0,
                    enabled: false,
                    active_revision: None,
                    schedule: None,
                    webhook: None,
                    activation: None,
                    created_ms: now,
                    updated_ms: now,
                },
                None => {
                    return Err(OrchError::NotAllowed(
                        "a new automation has no prior revision".into(),
                    ))
                }
            };
            let legacy = if let Some((schedule, policies)) = &source {
                let current: ScheduleRecord = serde_json::from_value(tx.get_record(SCHEDULE, &schedule.id)?.ok_or_else(|| OrchError::NotAllowed("the schedule is unavailable".into()))?)?;
                if current.version != schedule.version || current.target.is_some() || head.active_revision.is_some() || (id.is_some() && head.schedule.as_deref() != Some(schedule.id.as_str())) || tx.records_matching(automation::DEFINITION, "schedule", &schedule.id)?.iter().any(|entry| entry["id"].as_str() != Some(head.id.as_str())) { return Err(OrchError::NotAllowed("the schedule changed or already has an automation; reload before saving".into())); }
                head.schedule = Some(schedule.id.clone());
                Some(policies.clone())
            } else if head.revision > 0 { revision(tx, &head.id, head.revision)?.legacy } else { None };
            if let Some(policies) = &legacy {
                if !draft.steps.first().is_some_and(|step| step.id == policies.step && matches!(&step.action, automation::StepAction::Agent { target: WorkTarget::Agent { .. }, .. })) { return Err(OrchError::NotAllowed("keep the original agent step first to preserve this schedule's task policies".into())); }
            }
            head.revision = head
                .revision
                .checked_add(1)
                .ok_or_else(|| OrchError::NotAllowed("the revision limit was reached".into()))?;
            head.updated_ms = now;
            let mut revision = Revision {
                legacy,
                automation: head.id.clone(),
                number: head.revision,
                digest: String::new(),
                draft,
                created_ms: now,
            };
            revision.digest = revision.computed_digest()?;
            let receipt = SaveReceipt {
                automation: head.id.clone(),
                revision: head.revision,
                digest: revision.digest.clone(),
            };
            tx.insert_record(
                automation::REVISION,
                &format!("{}/{}", head.id, head.revision),
                &serde_json::to_value(&revision)?,
            )?;
            tx.put_record(
                automation::DEFINITION,
                &head.id,
                &serde_json::to_value(&head)?,
            )?;
            tx.insert_record(
                SAVE,
                &key,
                &serde_json::to_value(Saved {
                    digest,
                    receipt: Some(receipt.clone()),
                })?,
            )?;
            Ok::<_, OrchError>(receipt)
        })?;
        self.inner.emit(EventKind::Automation {
            automation: receipt.automation.clone(),
            run: None,
        });
        drop(state);
        Ok(receipt)
    }

    pub fn cancel_automation_save(
        &self,
        request: &str,
        id: Option<&str>,
        expected: Option<u64>,
        draft: Draft,
    ) -> Result<Option<SaveReceipt>, OrchError> {
        self.cancel_automation_save_source(request, id, expected, draft, None)
    }

    pub fn cancel_automation_save_source(
        &self,
        request: &str,
        id: Option<&str>,
        expected: Option<u64>,
        draft: Draft,
        adoption: Option<automation::LegacyAdoption>,
    ) -> Result<Option<SaveReceipt>, OrchError> {
        if request.is_empty() || request.len() > 256 {
            return Err(OrchError::NotAllowed(
                "cancelling a save needs its bounded request key".into(),
            ));
        }
        let draft = match draft.storage_validated() {
            Ok(draft) => draft,
            Err(_) => return Ok(None),
        };
        let project = self.project().id.as_str();
        let digest = save_digest(id, expected, &draft, adoption.as_ref())?;
        let key = format!(
            "as-{}",
            blake3::hash(&serde_json::to_vec(&(project, request))?).to_hex()
        );
        self.inner.engine.store.transact_records(project, |tx| {
            if let Some(value) = tx.get_record(SAVE, &key)? {
                let saved: Saved = serde_json::from_value(value)?;
                if saved.digest != digest {
                    return Err(OrchError::NotAllowed(
                        "this save request names another edit".into(),
                    ));
                }
                return Ok(saved.receipt);
            }
            tx.insert_record(
                SAVE,
                &key,
                &serde_json::to_value(Saved {
                    digest,
                    receipt: None,
                })?,
            )?;
            Ok(None)
        })
    }

    pub fn automation(&self, id: &str) -> Result<Entry, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let head: Definition = serde_json::from_value(
                    tx.get_record(automation::DEFINITION, id)?.ok_or_else(|| {
                        OrchError::NotAllowed("the automation is unavailable".into())
                    })?,
                )?;
                let revision = revision(tx, &head.id, head.revision)?;
                Ok(Entry {
                    active: head
                        .active_revision
                        .map(|number| self::revision(tx, &head.id, number))
                        .transpose()?,
                    last_start: tx
                        .get_record(automation::START_RECEIPT, &head.id)?
                        .map(serde_json::from_value)
                        .transpose()?,
                    pending_start: tx
                        .get_record(automation::MANUAL_START, &head.id)?
                        .map(serde_json::from_value)
                        .transpose()?,
                    webhook_availability: webhook_availability(tx, &head)?,
                    definition: head,
                    revision,
                })
            })
    }

    pub fn automation_page(&self, cursor: Option<&str>, limit: usize) -> Result<Page, OrchError> {
        let limit = limit.clamp(1, 100);
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let mut heads = tx
                    .records_page(automation::DEFINITION, "updated_ms", cursor, limit + 1)?
                    .into_iter()
                    .map(serde_json::from_value::<Definition>)
                    .collect::<Result<Vec<_>, _>>()?;
                let next = if heads.len() > limit {
                    Some(serde_json::to_string(&(
                        heads[limit - 1].updated_ms,
                        &heads[limit - 1].id,
                    ))?)
                } else {
                    None
                };
                heads.truncate(limit);
                let entries = heads
                    .into_iter()
                    .map(|head| {
                        Ok(Entry {
                            active: head
                                .active_revision
                                .map(|number| self::revision(tx, &head.id, number))
                                .transpose()?,
                            last_start: tx
                                .get_record(automation::START_RECEIPT, &head.id)?
                                .map(serde_json::from_value)
                                .transpose()?,
                            pending_start: tx
                                .get_record(automation::MANUAL_START, &head.id)?
                                .map(serde_json::from_value)
                                .transpose()?,
                            webhook_availability: webhook_availability(tx, &head)?,
                            revision: revision(tx, &head.id, head.revision)?,
                            definition: head,
                        })
                    })
                    .collect::<Result<Vec<_>, OrchError>>()?;
                Ok(Page { entries, next })
            })
    }

    pub fn automation_layout(&self, id: &str) -> Result<Layout, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                if tx.get_record(automation::DEFINITION, id)?.is_none() {
                    return Err(OrchError::NotAllowed(
                        "the automation is unavailable".into(),
                    ));
                }
                Ok(tx
                    .get_record(automation::LAYOUT, id)?
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default())
            })
    }

    pub fn save_automation_layout(
        &self,
        id: &str,
        expected: u64,
        mut layout: Layout,
    ) -> Result<Layout, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let head: Definition = serde_json::from_value(
                    tx.get_record(automation::DEFINITION, id)?.ok_or_else(|| {
                        OrchError::NotAllowed("the automation is unavailable".into())
                    })?,
                )?;
                let current: Layout = tx
                    .get_record(automation::LAYOUT, id)?
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default();
                if current.version != expected {
                    return Err(OrchError::NotAllowed(
                        "the canvas changed; reload before moving its steps".into(),
                    ));
                }
                layout
                    .validate(&revision(tx, id, head.revision)?)
                    .map_err(OrchError::NotAllowed)?;
                layout.version = expected.checked_add(1).ok_or_else(|| {
                    OrchError::NotAllowed("the layout version limit was reached".into())
                })?;
                tx.put_record(automation::LAYOUT, id, &serde_json::to_value(&layout)?)?;
                Ok(layout)
            })
    }

    pub fn automation_active_permission(
        &self,
        id: &str,
    ) -> Result<Option<automation::Activation>, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let head: Definition = serde_json::from_value(
                    tx.get_record(automation::DEFINITION, id)?.ok_or_else(|| {
                        OrchError::NotAllowed("the automation is unavailable".into())
                    })?,
                )?;
                let Some(activation) = head.activation else {
                    return Ok(None);
                };
                let prepared: PreparedActivation =
                    serde_json::from_value(tx.get_record(ACTIVATION, &activation)?.ok_or_else(
                        || OrchError::NotAllowed("the active permission is unavailable".into()),
                    )?)?;
                if prepared.value.automation != id {
                    return Err(OrchError::NotAllowed(
                        "the permission belongs to another automation".into(),
                    ));
                }
                Ok(Some(prepared.value))
            })
    }

    pub fn automation_run(&self, id: &str) -> Result<automation::Run, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                serde_json::from_value(tx.get_record(automation::RUN, id)?.ok_or_else(|| {
                    OrchError::NotAllowed("the automation execution is unavailable".into())
                })?)
                .map_err(Into::into)
            })
    }
}

pub(super) const ACTIVATION: &str = "automation_activation";

#[derive(Serialize, Deserialize)]
pub(super) struct PreparedActivation {
    pub value: automation::Activation,
    pub observed: std::collections::BTreeMap<String, AgentSpec>,
}

impl Orchestrator {
    pub async fn prepare_automation_activation(
        &self,
        id: &str,
        expected: u64,
    ) -> Result<automation::Activation, OrchError> {
        let entry = self.automation(id)?;
        if entry.definition.revision != expected {
            return Err(OrchError::NotAllowed(
                "the automation changed; review its current revision".into(),
            ));
        }
        if let Some(issue) = entry.revision.draft.activation_issues().first() {
            return Err(OrchError::NotAllowed(issue.message.clone()));
        }
        let mut prepared = self
            .inner
            .prepare_automation_choices(&entry.revision)
            .await?;
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let current: Definition = serde_json::from_value(
                    tx.get_record(automation::DEFINITION, id)?.ok_or_else(|| {
                        OrchError::NotAllowed("the automation is unavailable".into())
                    })?,
                )?;
                super::legacy_automation::validate_source(tx, &entry.revision)?;
                if current.revision != expected {
                    return Err(OrchError::NotAllowed(
                        "the automation changed while preparing its choices".into(),
                    ));
                }
                for (name, spec) in &prepared.observed {
                    let current: AgentRecord = serde_json::from_value(
                        tx.get_record(AGENT, &format!("{}/{name}", self.project().id))?
                            .ok_or_else(|| OrchError::UnknownAgent(name.clone()))?,
                    )?;
                    if serde_json::to_value(&current.spec)? != serde_json::to_value(spec)?
                        || current.state == AgentState::Terminated
                    {
                        return Err(OrchError::NotAllowed(format!(
                            "{name} changed while preparing the automation"
                        )));
                    }
                }
                if matches!(entry.revision.draft.trigger, automation::Trigger::Webhook) {
                    let endpoint = super::webhooks::endpoint(
                        tx,
                        current.webhook.as_deref().ok_or_else(|| {
                            OrchError::NotAllowed("connect this webhook before activation".into())
                        })?,
                    )?;
                    if endpoint.draft.availability == automation::WebhookAvailability::Pending
                        || endpoint
                            .draft
                            .verified_ms
                            .is_none_or(|verified| now_ms().saturating_sub(verified) > 300_000)
                    {
                        return Err(OrchError::NotAllowed(
                            "verify this webhook connection before activation".into(),
                        ));
                    }
                    prepared.value.webhook = Some(endpoint.draft_snapshot());
                    prepared.value.digest =
                        prepared.value.computed_digest(&entry.revision.digest)?;
                }
                tx.insert_record(
                    ACTIVATION,
                    &prepared.value.id,
                    &serde_json::to_value(&prepared)?,
                )?;
                Ok::<_, OrchError>(())
            })?;
        Ok(prepared.value)
    }
}

impl Inner {
    async fn freeze_model_selection(
        &self,
        lane: &str,
        spec: Option<&str>,
    ) -> Result<String, OrchError> {
        let resolved = self.models.resolve(lane, spec).await;
        let selected = resolved.and_then(|models| {
            models.require_host_tools()?;
            Ok(self.models.selection_spec(&models))
        });
        self.models.close(lane).await;
        selected
    }

    pub(super) async fn prepare_automation_choices(
        &self,
        revision: &Revision,
    ) -> Result<PreparedActivation, OrchError> {
        use ark_tools::Adapter;
        use std::collections::{BTreeMap, BTreeSet};
        let issues = self
            .engine
            .store
            .transact_records(self.project.id.as_str(), |tx| {
                draft_issues(tx, self.project.id.as_str(), &revision.draft)
            })?;
        if let Some(issue) = issues.first() {
            return Err(OrchError::NotAllowed(issue.message.clone()));
        }
        let agents = self
            .engine
            .store
            .transact_records(self.project.id.as_str(), |tx| {
                tx.list_records(AGENT)?
                    .into_iter()
                    .map(|value| {
                        let agent: AgentRecord = serde_json::from_value(value)?;
                        Ok::<_, OrchError>((agent.name.clone(), agent))
                    })
                    .collect::<Result<BTreeMap<_, _>, _>>()
            })?;
        let delegate = revision
            .draft
            .authority
            .actions
            .iter()
            .any(|action| action.action == tools::DELEGATE);
        let mut wanted = BTreeSet::new();
        for step in &revision.draft.steps {
            match &step.action {
                automation::StepAction::Agent {
                    target: WorkTarget::Agent { name },
                    ..
                } => {
                    wanted.insert(name.clone());
                }
                automation::StepAction::Agent {
                    target: WorkTarget::General { .. },
                    ..
                } if delegate => {
                    wanted.extend(
                        agents
                            .values()
                            .filter(|agent| agent.state != AgentState::Terminated)
                            .map(|agent| agent.name.clone()),
                    );
                }
                _ => {}
            }
        }
        if delegate {
            loop {
                let before = wanted.len();
                for name in wanted.clone() {
                    let agent = agents
                        .get(&name)
                        .ok_or_else(|| OrchError::UnknownAgent(name.clone()))?;
                    wanted.extend(
                        agents
                            .values()
                            .filter(|candidate| {
                                candidate.state != AgentState::Terminated
                                    && agent.spec.may_delegate(&candidate.name)
                            })
                            .map(|candidate| candidate.name.clone()),
                    );
                }
                if wanted.len() == before {
                    break;
                }
                if wanted.len() > 32 {
                    return Err(OrchError::NotAllowed(
                        "one automation can authorize at most 32 reachable agents".into(),
                    ));
                }
            }
        }
        if wanted.len() > 32 {
            return Err(OrchError::NotAllowed(
                "one automation can authorize at most 32 reachable agents".into(),
            ));
        }
        let mut observed = BTreeMap::new();
        let mut frozen = BTreeMap::new();
        let activation = format!("ag-{}", uuid::Uuid::new_v4().simple());
        for name in wanted {
            let agent = agents
                .get(&name)
                .ok_or_else(|| OrchError::UnknownAgent(name.clone()))?;
            if agent.state == AgentState::Terminated {
                return Err(OrchError::Terminated(name));
            }
            if agent
                .spec
                .profile_name
                .as_ref()
                .is_some_and(|name| !self.profiles.iter().any(|profile| &profile.name == name))
            {
                return Err(OrchError::NotAllowed(
                    "the agent profile is unavailable".into(),
                ));
            }
            if self.worker_for(agent.spec.model.as_deref())?.is_some() {
                return Err(OrchError::NotAllowed(
                    "this agent program has no verified automation containment contract".into(),
                ));
            }
            let lane = format!(
                "{}/automation/preflight/{activation}/agent/{name}",
                self.project.id
            );
            let model = self
                .freeze_model_selection(&lane, agent.spec.model.as_deref())
                .await?;
            observed.insert(name.clone(), agent.spec.clone());
            let mut spec = agent.spec.clone();
            spec.model = Some(model);
            frozen.insert(name, spec);
        }
        let mut selections = BTreeMap::new();
        for step in &revision.draft.steps {
            let automation::StepAction::Agent { target, .. } = &step.action else {
                continue;
            };
            let selected = match target {
                WorkTarget::Agent { name } => {
                    let spec = frozen
                        .get(name)
                        .ok_or_else(|| OrchError::UnknownAgent(name.clone()))?;
                    automation::FrozenSelection {
                        model: spec.model.clone().ok_or_else(|| {
                            OrchError::NotAllowed("the frozen model is missing".into())
                        })?,
                        agent: Some(spec.clone()),
                    }
                }
                WorkTarget::General { model } => {
                    if self.worker_for(model.as_deref())?.is_some() {
                        return Err(OrchError::NotAllowed(
                            "an external program cannot replace General in an automation".into(),
                        ));
                    }
                    let lane = format!(
                        "{}/automation/preflight/{activation}/step/{}",
                        self.project.id, step.id
                    );
                    let model = self.freeze_model_selection(&lane, model.as_deref()).await?;
                    automation::FrozenSelection { model, agent: None }
                }
            };
            selections.insert(step.id.clone(), selected);
        }
        let offered = ark_sdk::offered_tools(&self.engine, &self.per_agent);
        let agent_tools = tools::AgentTools {
            orch: self.me.clone(),
            agent: String::new(),
            task: String::new(),
            delegates: frozen
                .iter()
                .map(|(name, spec)| (name.clone(), spec.role.clone()))
                .collect(),
            contacts: vec![],
            can_create: false,
            schedule: None,
            comes_back: false,
        };
        let descriptors: Vec<_> = agent_tools
            .actions()
            .into_iter()
            .map(super::automation_children::descriptor)
            .collect();
        let mut selected = BTreeMap::new();
        for allowed in &revision.draft.authority.actions {
            if allowed.action.starts_with("agents.")
                && !matches!(allowed.action.as_str(), tools::DELEGATE | tools::WAIT)
            {
                return Err(OrchError::NotAllowed(
                    "this agent operation cannot yet preserve automation authority".into(),
                ));
            }
            let descriptor = if allowed.action.starts_with("agents.") {
                descriptors
                    .iter()
                    .find(|descriptor| descriptor.name == allowed.action)
                    .cloned()
            } else {
                offered.descriptor(&allowed.action)
            }
            .ok_or_else(|| {
                OrchError::NotAllowed(format!("{} is not an available action", allowed.action))
            })?;
            if descriptor.person_only {
                return Err(OrchError::NotAllowed(
                    "a person-only action cannot be granted to an automation".into(),
                ));
            }
            if allowed.unattended
                && descriptor.risk == ark_core::RiskTier::Mutate
                && (!descriptor.auto_eligible || descriptor.ask_each_time)
            {
                return Err(OrchError::NotAllowed(format!(
                    "{} requires a live approval",
                    allowed.action
                )));
            }
            if allowed
                .inputs
                .values()
                .any(|bound| matches!(bound, automation::Bound::WorkspacePath { .. }))
            {
                return Err(OrchError::NotAllowed(format!(
                    "{} has no verified contract for the requested folder bound",
                    allowed.action
                )));
            }
            selected.insert(allowed.action.clone(), descriptor);
        }
        let mut value = automation::Activation {
            id: activation,
            automation: revision.automation.clone(),
            revision: revision.number,
            digest: String::new(),
            selections,
            agents: frozen,
            descriptors: selected,
            phase: automation::ActivationPhase::Prepared,
            created_ms: now_ms(),
            webhook: None,
        };
        value.digest = value.computed_digest(&revision.digest)?;
        Ok(PreparedActivation { observed, value })
    }
}

impl Orchestrator {
    pub fn preview_automation(&self, draft: Draft) -> Result<automation::Preview, OrchError> {
        let draft = draft.validated().map_err(OrchError::NotAllowed)?;
        let mut next = Vec::new();
        if let automation::Trigger::Schedule { when } = &draft.trigger {
            let mut after = now_ms();
            for _ in 0..3 {
                let Some(at_ms) = when.next_after(after) else {
                    break;
                };
                next.push(automation::Occurrence {
                    at_ms,
                    label: when.label(at_ms),
                });
                after = at_ms;
                if when.once {
                    break;
                }
            }
        }
        Ok(automation::Preview {
            digest: draft.digest()?,
            trigger: draft.trigger,
            next,
        })
    }

    pub fn automation_history(
        &self,
        id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<automation::RunPage, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                if tx.get_record(automation::DEFINITION, id)?.is_none() {
                    return Err(OrchError::NotAllowed(
                        "the automation is unavailable".into(),
                    ));
                }
                let limit = limit.clamp(1, 100);
                let mut rows = tx
                    .records_page_matching(
                        automation::RUN,
                        "automation",
                        id,
                        "created_ms",
                        cursor,
                        limit + 1,
                    )?
                    .into_iter()
                    .map(serde_json::from_value::<automation::Run>)
                    .collect::<Result<Vec<_>, _>>()?;
                let next = if rows.len() > limit {
                    Some(serde_json::to_string(&(
                        rows[limit - 1].created_ms,
                        &rows[limit - 1].id,
                    ))?)
                } else {
                    None
                };
                rows.truncate(limit);
                let runs = rows
                    .into_iter()
                    .map(|run| automation::RunSummary {
                        id: run.id,
                        work: run.work,
                        conversation: run.conversation,
                        revision: run.revision.number,
                        origin: run.origin,
                        phase: run.phase,
                        error: run.error,
                        created_ms: run.created_ms,
                        updated_ms: run.updated_ms,
                    })
                    .collect();
                Ok(automation::RunPage { runs, next })
            })
    }
}
