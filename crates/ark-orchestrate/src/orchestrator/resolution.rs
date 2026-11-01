use super::*;
use serde::{Deserialize, Serialize};

const RESOLUTION: &str = "orch_work_resolution";

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkRunRef {
    Automation { run: String },
    Task { task: String },
    General { receipt: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkFailure {
    pub run: WorkRunRef,
    pub digest: String,
    pub error: String,
    pub resolved: bool,
    pub interrupted: bool,
}

#[derive(Serialize, Deserialize)]
struct Resolution {
    work: String,
    user: String,
    run: WorkRunRef,
    digest: String,
    at_ms: u64,
}

fn identity(user: &str, work: &str, run: &WorkRunRef) -> Result<String, OrchError> {
    Ok(format!(
        "wf-{}",
        blake3::hash(&serde_json::to_vec(&(user, work, run))?).to_hex()
    ))
}

fn failure(
    tx: &ark_store::RecordTransaction<'_>,
    run: WorkRunRef,
) -> Result<Option<WorkFailure>, OrchError> {
    let (value, error, interrupted) = match &run {
        WorkRunRef::Automation { run: id } => {
            let Some(value) = tx.get_record(crate::automation::RUN, id)? else {
                return Ok(None);
            };
            let execution: crate::automation::Run = serde_json::from_value(value)?;
            if !matches!(
                execution.phase,
                crate::automation::RunPhase::Failed | crate::automation::RunPhase::Interrupted
            ) {
                return Ok(None);
            }
            (
                serde_json::to_value((&execution.phase, &execution.error, &execution.output))?,
                execution
                    .error
                    .unwrap_or_else(|| "The execution stopped".into()),
                execution.phase == crate::automation::RunPhase::Interrupted,
            )
        }
        WorkRunRef::Task { task } => {
            let Some(value) = tx.get_record(TASK, task)? else {
                return Ok(None);
            };
            let task: TaskRecord = serde_json::from_value(value)?;
            if task.state != TaskState::Failed {
                return Ok(None);
            }
            (
                serde_json::to_value((
                    &task.state,
                    &task.finished_ms,
                    &task.error,
                    &task.result,
                    &task.outputs,
                ))?,
                task.error.unwrap_or_else(|| "Could not finish".into()),
                false,
            )
        }
        WorkRunRef::General { receipt } => {
            let Some(value) = tx.get_record(admission::SUBMISSION, receipt)? else {
                return Ok(None);
            };
            let submission: admission::Submission = serde_json::from_value(value)?;
            let result = general::result(tx, &submission.receipt)?;
            let phase = match &result {
                Some(result) => result.state,
                None => general::state(tx, &submission.receipt)?,
            };
            let empty = phase == GeneralWorkState::Completed
                && !result.as_ref().is_some_and(|result| {
                    result
                        .answer
                        .as_ref()
                        .is_some_and(|answer| !answer.trim().is_empty())
                        || !result.outputs.is_empty()
                });
            if !matches!(
                phase,
                GeneralWorkState::Failed | GeneralWorkState::Interrupted
            ) && !empty
            {
                return Ok(None);
            }
            let error = result
                .as_ref()
                .and_then(|result| result.error.clone())
                .unwrap_or_else(|| {
                    if empty {
                        "No result was saved".into()
                    } else {
                        "The conversation was interrupted".into()
                    }
                });
            (
                serde_json::to_value((&phase, &result))?,
                error,
                phase == GeneralWorkState::Interrupted,
            )
        }
    };
    let digest = format!("v1:{}", blake3::hash(&serde_json::to_vec(&value)?).to_hex());
    Ok(Some(WorkFailure {
        run,
        digest,
        error,
        resolved: false,
        interrupted,
    }))
}

pub(super) fn failures(
    tx: &ark_store::RecordTransaction<'_>,
    user: &str,
    item: &WorkItem,
    tasks: &[TaskRecord],
    general: &[(
        WorkReceipt,
        GeneralWorkState,
        Option<general::GeneralWorkResult>,
    )],
) -> Result<Vec<WorkFailure>, OrchError> {
    let refs = tasks
        .iter()
        .filter(|task| task.run.is_none())
        .map(|task| WorkRunRef::Task {
            task: task.id.clone(),
        })
        .chain(general.iter().map(|(receipt, _, _)| WorkRunRef::General {
            receipt: receipt.id.clone(),
        }))
        .chain(
            item.run
                .iter()
                .map(|run| WorkRunRef::Automation { run: run.clone() }),
        );
    let mut failures = Vec::new();
    for run in refs {
        let Some(mut failed) = failure(tx, run)? else {
            continue;
        };
        if let Some(value) = tx.get_record(RESOLUTION, &identity(user, &item.id, &failed.run)?)? {
            let resolved: Resolution = serde_json::from_value(value)?;
            failed.resolved = resolved.work == item.id
                && resolved.user == user
                && resolved.run == failed.run
                && resolved.digest == failed.digest;
        }
        failures.push(failed);
    }
    Ok(failures)
}

impl Orchestrator {
    /// The caller authenticates user. The digest is a precondition, never authority.
    pub fn resolve_work_failure(
        &self,
        user: &str,
        work: &str,
        run: WorkRunRef,
        expected: &str,
        resolved: bool,
    ) -> Result<(), OrchError> {
        if user.is_empty() || user.len() > 256 {
            return Err(OrchError::NotAllowed("a user identity is required".into()));
        }
        self.inner.with_work(|data, tx| {
            let item = data
                .items
                .get(work)
                .ok_or_else(|| OrchError::NotAllowed("the work item is unavailable".into()))?;
            let belongs = match &run {
                WorkRunRef::Automation { run } => item.run.as_ref() == Some(run),
                WorkRunRef::Task { task } => {
                    let mut cursor = Some(task.as_str());
                    let mut seen = std::collections::HashSet::new();
                    let mut belongs = false;
                    while let Some(id) = cursor {
                        if !seen.insert(id) {
                            break;
                        }
                        let Some(task) = data.tasks.get(id) else {
                            break;
                        };
                        if item.tasks.iter().any(|root| root == id)
                            || task.item.as_deref() == Some(work)
                        {
                            belongs = true;
                            break;
                        }
                        cursor = task.parent.as_deref();
                    }
                    belongs
                }
                WorkRunRef::General { receipt } => {
                    match tx.get_record(admission::SUBMISSION, receipt)? {
                        Some(value) => {
                            serde_json::from_value::<admission::Submission>(value)?
                                .receipt
                                .work
                                == work
                        }
                        None => false,
                    }
                }
            };
            if !belongs {
                return Err(OrchError::NotAllowed(
                    "this execution belongs to another work item".into(),
                ));
            }
            let current = failure(tx, run.clone())?.ok_or_else(|| {
                OrchError::NotAllowed("this execution no longer has that failure".into())
            })?;
            if current.digest != expected {
                return Err(OrchError::NotAllowed(
                    "the failure changed; review its current result".into(),
                ));
            }
            let id = identity(user, work, &run)?;
            if resolved {
                tx.put_record(
                    RESOLUTION,
                    &id,
                    &serde_json::to_value(Resolution {
                        work: work.to_string(),
                        user: user.to_string(),
                        run,
                        digest: current.digest,
                        at_ms: now_ms(),
                    })?,
                )?;
            } else {
                tx.delete_record(RESOLUTION, &id)?;
            }
            Ok((
                (),
                vec![EventKind::Work {
                    item: item.clone(),
                    by: user.to_string(),
                }],
            ))
        })
    }
}
