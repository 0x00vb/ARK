use super::*;
use crate::automation::{self, Run, RunQuestion};
use serde::{Deserialize, Serialize};

const QUESTION_INDEX: &str = "automation_question_index";

#[derive(Serialize, Deserialize, PartialEq)]
struct QuestionIndex {
    project: String,
    run: String,
    question: String,
    digest: String,
}

pub(super) fn retain_question(
    tx: &ark_store::RecordTransaction<'_>,
    run: &Run,
    question: &RunQuestion,
) -> Result<(), OrchError> {
    let pointer = QuestionIndex {
        project: run.project.clone(),
        run: run.id.clone(),
        question: question.id.clone(),
        digest: question.digest.clone(),
    };
    if let Some(prior) = tx.get_record(QUESTION_INDEX, &question.id)? {
        if serde_json::from_value::<QuestionIndex>(prior)? != pointer {
            return Err(OrchError::NotAllowed(
                "the retained approval changed identity".into(),
            ));
        }
        return Ok(());
    }
    tx.insert_record(
        QUESTION_INDEX,
        &question.id,
        &serde_json::to_value(pointer)?,
    )?;
    Ok(())
}

fn runs_in_states(
    store: &ark_store::Store,
    states: &[&str],
    since: u64,
) -> Result<Vec<Run>, OrchError> {
    store
        .records_in_states(automation::RUN, "phase", states, "updated_ms", since)?
        .into_iter()
        .map(|value| serde_json::from_value(value).map_err(OrchError::from))
        .collect()
}

pub fn automation_attention_runs(
    store: &ark_store::Store,
    since: u64,
) -> Result<Vec<Run>, OrchError> {
    let mut runs = runs_in_states(store, &["pending", "running", "waiting_approval"], 0)?;
    runs.extend(runs_in_states(
        store,
        &["completed", "failed", "cancelled", "interrupted"],
        since,
    )?);
    Ok(runs)
}

pub fn automation_projects_with_work(store: &ark_store::Store) -> Result<Vec<String>, OrchError> {
    let mut projects = runs_in_states(store, &["pending", "running", "waiting_approval"], 0)?
        .into_iter()
        .map(|run| run.project)
        .collect::<Vec<_>>();
    projects.sort();
    projects.dedup();
    Ok(projects)
}

pub fn stored_automation_run(store: &ark_store::Store, id: &str) -> Result<Option<Run>, OrchError> {
    store
        .get_record(automation::RUN, id)?
        .map(serde_json::from_value)
        .transpose()
        .map_err(OrchError::from)
}

pub fn automation_question(
    store: &ark_store::Store,
    id: &str,
) -> Result<Option<(Run, RunQuestion)>, OrchError> {
    if !id.starts_with("aq-") || id.len() > 128 {
        return Ok(None);
    }
    if let Some(pointer) = store.get_record(QUESTION_INDEX, id)? {
        let pointer: QuestionIndex = serde_json::from_value(pointer)?;
        let run: Run = serde_json::from_value(
            store
                .get_record(automation::RUN, &pointer.run)?
                .ok_or_else(|| {
                    OrchError::NotAllowed("the retained approval's execution is unavailable".into())
                })?,
        )?;
        if run.project != pointer.project || pointer.question != id {
            return Err(OrchError::NotAllowed(
                "the retained approval changed ownership".into(),
            ));
        }
        let question = run
            .steps
            .iter()
            .filter_map(|step| step.question.as_ref())
            .chain(run.questions.values())
            .find(|q| q.id == id && q.digest == pointer.digest)
            .cloned()
            .ok_or_else(|| {
                OrchError::NotAllowed("the retained approval changed identity".into())
            })?;
        return Ok(Some((run, question)));
    }
    for run in runs_in_states(store, &["pending", "running", "waiting_approval"], 0)? {
        if let Some(question) = run
            .steps
            .iter()
            .filter_map(|step| step.question.as_ref())
            .chain(run.questions.values())
            .find(|q| q.id == id)
            .cloned()
        {
            return Ok(Some((run, question)));
        }
    }
    Ok(None)
}
