use super::*;

#[derive(Clone, Debug, serde::Serialize)]
pub struct WorkTemplate {
    pub objective: String,
    pub artifacts: Vec<String>,
    pub target: WorkTarget,
}

impl Orchestrator {
    pub fn work_template(
        &self,
        work: &str,
        receipt: Option<&str>,
    ) -> Result<WorkTemplate, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let submissions = tx
                    .records_matching(admission::SUBMISSION, "receipt.work", work)?
                    .into_iter()
                    .map(serde_json::from_value::<admission::Submission>)
                    .collect::<Result<Vec<_>, _>>()?;
                let conversation = receipt
                    .map(|id| {
                        submissions
                            .iter()
                            .find(|source| source.receipt.id == id)
                            .map(|source| source.receipt.conversation.as_str())
                            .ok_or_else(|| {
                                OrchError::NotAllowed(
                                    "the selected submission belongs to another work item".into(),
                                )
                            })
                    })
                    .transpose()?;
                let source = submissions
                    .iter()
                    .filter(|source| {
                        conversation.is_none_or(|id| source.receipt.conversation == id)
                    })
                    .min_by_key(|source| (source.receipt.sequence, source.receipt.created_ms))
                    .ok_or_else(|| {
                        OrchError::NotAllowed("the original work submission is unavailable".into())
                    })?;
                Ok(WorkTemplate {
                    objective: source.spec.objective.clone(),
                    artifacts: source.spec.artifacts.clone(),
                    target: source.target.clone(),
                })
            })
    }
}

pub struct WorkEntry {
    pub delivery_error: Option<String>,
    pub receipts: Vec<WorkReceipt>,
    pub targets: std::collections::BTreeMap<String, WorkTarget>,
    pub run: Option<crate::automation::Run>,
    pub item: WorkItem,
    pub tasks: Vec<TaskRecord>,
    pub missing_tasks: Vec<String>,
    pub general: Vec<(
        WorkReceipt,
        GeneralWorkState,
        Option<general::GeneralWorkResult>,
    )>,
    pub conversation_missing: bool,
    pub origins: Vec<String>,
    pub failures: Vec<resolution::WorkFailure>,
    pub target: Option<WorkTarget>,
    pub start_receipt: Option<String>,
}

pub struct WorkPage {
    pub entries: Vec<WorkEntry>,
    pub next: Option<String>,
}

fn entry(
    tx: &ark_store::RecordTransaction<'_>,
    item: WorkItem,
    user: &str,
) -> Result<WorkEntry, OrchError> {
    let submissions = tx
        .records_matching(admission::SUBMISSION, "receipt.work", &item.id)?
        .into_iter()
        .map(serde_json::from_value::<admission::Submission>)
        .collect::<Result<Vec<_>, _>>()?;
    let mut pending = item.tasks.clone();
    let linked = tx
        .records_matching(TASK, "item", &item.id)?
        .into_iter()
        .map(serde_json::from_value::<TaskRecord>)
        .collect::<Result<Vec<_>, _>>()?;
    pending.extend(linked.into_iter().map(|task| task.id));
    let mut tasks = Vec::new();
    let mut missing_tasks = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(value) = tx.get_record(TASK, &id)? {
            let task: TaskRecord = serde_json::from_value(value)?;
            pending.extend(
                tx.records_matching(TASK, "parent", &id)?
                    .into_iter()
                    .map(serde_json::from_value::<TaskRecord>)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .map(|child| child.id),
            );
            tasks.push(task);
        } else {
            missing_tasks.push(id);
        }
    }
    tasks.sort_by_key(|task| (task.created_ms, task.id.clone()));
    let conversation_missing = match &item.conversation {
        Some(id) => tx.get_session(id)?.is_none(),
        None => false,
    };
    let mut origins: Vec<_> = submissions
        .iter()
        .map(|submission| submission.receipt.origin.clone())
        .collect();
    origins.sort();
    origins.dedup();
    let latest = submissions
        .iter()
        .filter(|submission| submission.receipt.parent.is_none())
        .max_by_key(|submission| {
            (
                submission.receipt.sequence,
                submission.receipt.created_ms,
                &submission.receipt.id,
            )
        });
    let target = latest.map(|submission| submission.target.clone());
    let start_receipt = match latest {
        Some(submission) if submission.receipt.turn.is_some() => {
            (general::state(tx, &submission.receipt)? == GeneralWorkState::Pending)
                .then(|| submission.receipt.id.clone())
        }
        Some(submission) => tasks
            .iter()
            .find(|task| Some(&task.id) == submission.receipt.task.as_ref())
            .filter(|task| {
                !task.state.is_final()
                    && task.started_ms.is_none()
                    && task.execution_owner.is_none()
            })
            .map(|_| submission.receipt.id.clone()),
        None => None,
    };
    let mut general = submissions
        .iter()
        .filter(|submission| submission.receipt.turn.is_some())
        .map(|submission| {
            let result = general::result(tx, &submission.receipt)?;
            let state = match &result {
                Some(result) => result.state,
                None => general::state(tx, &submission.receipt)?,
            };
            Ok::<_, OrchError>((submission.receipt.clone(), state, result))
        })
        .collect::<Result<Vec<_>, _>>()?;
    general.sort_by_key(|(receipt, _, _)| (receipt.turn, receipt.created_ms));
    let failures = resolution::failures(tx, user, &item, &tasks, &general)?;
    let run: Option<crate::automation::Run> = item
        .run
        .as_ref()
        .map(|id| -> Result<_, OrchError> {
            Ok(serde_json::from_value(
                tx.get_record(crate::automation::RUN, id)?.ok_or_else(|| {
                    OrchError::NotAllowed("the automation execution is missing".into())
                })?,
            )?)
        })
        .transpose()?;
    let delivery_error = if let Some(run) = &run {
        tx.get_record(super::automation_reports::REPORT, &run.id)?
            .map(serde_json::from_value::<super::automation_reports::ReportDelivery>)
            .transpose()?
            .and_then(|delivery| delivery.error)
    } else {
        None
    };
    Ok(WorkEntry {
        delivery_error,
        targets: submissions
            .iter()
            .map(|submission| (submission.receipt.id.clone(), submission.target.clone()))
            .collect(),
        receipts: submissions
            .into_iter()
            .filter(|submission| submission.receipt.parent.is_none())
            .map(|submission| submission.receipt)
            .collect(),
        run,
        item,
        tasks,
        missing_tasks,
        general,
        conversation_missing,
        origins,
        failures,
        target,
        start_receipt,
    })
}

impl Orchestrator {
    pub fn work_detail(&self, user: &str, id: &str) -> Result<WorkPage, OrchError> {
        if user.is_empty() {
            return Err(OrchError::NotAllowed("a user identity is required".into()));
        }
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let item = tx
                    .get_record(WORK, id)?
                    .ok_or_else(|| OrchError::NotAllowed("the work item is unavailable".into()))?;
                Ok(WorkPage {
                    entries: vec![entry(tx, serde_json::from_value(item)?, user)?],
                    next: None,
                })
            })
    }

    pub fn work_page(
        &self,
        user: &str,
        archived: bool,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<WorkPage, OrchError> {
        if user.is_empty() {
            return Err(OrchError::NotAllowed("a user identity is required".into()));
        }
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let limit = limit.clamp(1, 100);
                let mut after = cursor.map(str::to_string);
                let mut items = Vec::new();
                loop {
                    let batch = tx.records_page(WORK, "updated_ms", after.as_deref(), 100)?;
                    let finished = batch.len() < 100;
                    for value in batch {
                        let item: WorkItem = serde_json::from_value(value)?;
                        after = Some(serde_json::to_string(&(item.updated_ms, &item.id))?);
                        if item.archived_by.iter().any(|who| who == user) == archived {
                            items.push(item);
                        }
                        if items.len() > limit {
                            break;
                        }
                    }
                    if finished || items.len() > limit {
                        break;
                    }
                }
                let next = if items.len() > limit {
                    Some(serde_json::to_string(&(
                        items[limit - 1].updated_ms,
                        &items[limit - 1].id,
                    ))?)
                } else {
                    None
                };
                items.truncate(limit);
                let entries = items
                    .into_iter()
                    .map(|item| entry(tx, item, user))
                    .collect::<Result<_, _>>()?;
                Ok(WorkPage { entries, next })
            })
    }
}

impl WorkEntry {
    pub fn select_conversation(&mut self, receipt: &str) -> Result<WorkReceipt, OrchError> {
        let selected = self
            .receipts
            .iter()
            .find(|candidate| candidate.id == receipt)
            .ok_or_else(|| {
                OrchError::NotAllowed("the selected execution belongs to another work item".into())
            })?
            .clone();
        let latest = self
            .receipts
            .iter()
            .filter(|candidate| candidate.conversation == selected.conversation)
            .max_by_key(|candidate| (candidate.sequence, candidate.created_ms))
            .cloned()
            .ok_or_else(|| {
                OrchError::NotAllowed("the selected conversation is unavailable".into())
            })?;
        let mut tasks: std::collections::HashSet<_> = self
            .tasks
            .iter()
            .filter(|task| task.conversation.as_deref() == Some(&selected.conversation))
            .map(|task| task.id.clone())
            .collect();
        loop {
            let before = tasks.len();
            for task in &self.tasks {
                if task
                    .parent
                    .as_ref()
                    .is_some_and(|parent| tasks.contains(parent))
                {
                    tasks.insert(task.id.clone());
                }
            }
            if tasks.len() == before {
                break;
            }
        }
        self.tasks.retain(|task| tasks.contains(&task.id));
        self.item.tasks.retain(|task| tasks.contains(task));
        self.general
            .retain(|(receipt, _, _)| receipt.conversation == selected.conversation);
        self.receipts
            .retain(|receipt| receipt.conversation == selected.conversation);
        self.run = self
            .run
            .take()
            .filter(|run| run.conversation == selected.conversation);
        self.item.run = self.run.as_ref().map(|run| run.id.clone());
        if self.run.is_none() {
            self.delivery_error = None;
        }
        self.failures.retain(|failure| match &failure.run {
            WorkRunRef::Task { task } => tasks.contains(task),
            WorkRunRef::General { receipt } => self
                .receipts
                .iter()
                .any(|candidate| &candidate.id == receipt),
            WorkRunRef::Automation { run } => self
                .run
                .as_ref()
                .is_some_and(|candidate| &candidate.id == run),
        });
        self.target = self.targets.get(&latest.id).cloned();
        self.item.conversation = Some(selected.conversation);
        self.start_receipt = if latest.turn.is_some() {
            self.general
                .iter()
                .find(|(receipt, _, _)| receipt.id == latest.id)
                .filter(|(_, state, _)| *state == GeneralWorkState::Pending)
                .map(|_| latest.id.clone())
        } else {
            self.tasks
                .iter()
                .find(|task| Some(&task.id) == latest.task.as_ref())
                .filter(|task| {
                    !task.state.is_final()
                        && task.started_ms.is_none()
                        && task.execution_owner.is_none()
                })
                .map(|_| latest.id.clone())
        };
        Ok(latest)
    }
}
