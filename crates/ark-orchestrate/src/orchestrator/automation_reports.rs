use super::*;
use crate::automation::{Run, RunPhase};
use serde::{Deserialize, Serialize};

pub(super) const REPORT: &str = "automation_result_post";
#[derive(Serialize, Deserialize)]
pub(super) struct ReportDelivery {
    pub run: String,
    pub session: String,
    pub from: String,
    pub title: String,
    pub text: String,
    pub ok: bool,
    pub state: String,
    pub attempts: u32,
    pub retry_ms: u64,
    pub error: Option<String>,
}

pub(super) fn prepare(tx: &ark_store::RecordTransaction<'_>, run: &Run) -> Result<(), OrchError> {
    let Some(policies) = &run.revision.legacy else {
        return Ok(());
    };
    let Some(session) = &policies.report_to else {
        return Ok(());
    };
    if !matches!(run.phase, RunPhase::Completed | RunPhase::Failed) {
        return Ok(());
    }
    if tx.get_record(REPORT, &run.id)?.is_some() {
        return Ok(());
    }
    let from = run
        .revision
        .draft
        .steps
        .first()
        .and_then(|step| match &step.action {
            crate::automation::StepAction::Agent {
                target: WorkTarget::Agent { name },
                ..
            } => Some(name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| run.revision.draft.name.clone());
    let text = if run.phase == RunPhase::Failed {
        format!(
            "{}{}Could not finish: {}",
            run.output.text,
            if run.output.text.is_empty() {
                ""
            } else {
                "\n\n"
            },
            run.error.as_deref().unwrap_or("The execution stopped")
        )
    } else {
        run.output.text.clone()
    };
    tx.insert_record(
        REPORT,
        &run.id,
        &serde_json::to_value(ReportDelivery {
            run: run.id.clone(),
            session: session.clone(),
            from,
            title: clip(&run.revision.draft.name, 80),
            text,
            ok: run.phase == RunPhase::Completed,
            state: "pending".into(),
            attempts: 0,
            retry_ms: 0,
            error: None,
        })?,
    )?;
    Ok(())
}

impl Inner {
    pub(super) fn deliver_automation_reports(&self) -> Result<(), OrchError> {
        let reports =
            self.engine
                .store
                .transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                    tx.records_matching_due(REPORT, "state", "pending", "retry_ms", now_ms(), 100)?
                        .into_iter()
                        .map(serde_json::from_value::<ReportDelivery>)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(Into::into)
                })?;
        for report in reports
            .into_iter()
            .filter(|report| report.retry_ms <= now_ms())
        {
            let sent = ark_runtime::session::post_once(
                &self.engine,
                &report.session,
                Post::new(&report.from, &report.title, &report.text, report.ok).work(&report.run),
            );
            let outcome = self.engine.store.transact_records::<_, OrchError>(
                self.project.id.as_str(),
                |tx| {
                    let mut current: ReportDelivery =
                        serde_json::from_value(tx.get_record(REPORT, &report.run)?.ok_or_else(
                            || OrchError::NotAllowed("the result delivery is unavailable".into()),
                        )?)?;
                    if current.state != "pending" {
                        return Ok(());
                    }
                    match &sent {
                        Ok(_) => {
                            current.state = "delivered".into();
                            current.error = None;
                        }
                        Err(error) => {
                            current.attempts = current.attempts.saturating_add(1);
                            current.retry_ms = now_ms()
                                .saturating_add(5_000u64.saturating_mul(
                                    1 << current.attempts.saturating_sub(1).min(7),
                                ));
                            current.error = Some(error.to_string());
                        }
                    }
                    tx.put_record(REPORT, &report.run, &serde_json::to_value(current)?)?;
                    Ok(())
                },
            );
            outcome?;
            if let Err(error) = sent {
                tracing::warn!(run=%report.run,%error,"could not deliver the automation result into its conversation");
            }
        }
        Ok(())
    }
}
