//! Work is a projection of durable items, executions and open questions.
use std::collections::HashSet;

use ark_orchestrate::{ArtifactRef, GeneralWorkState, TaskState, WorkEntry, WorkPage};
use ark_sdk::prelude::ApprovalRequest;
use serde::Serialize;

use crate::{approval_choices, approval_title, fmt::clip, Choice, Labels};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Attention,
    InProgress,
    Waiting,
    Completed,
    Resolved,
    Failed,
    Cancelled,
    Interrupted,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Attention => "Needs your attention",
            Self::InProgress => "In progress",
            Self::Waiting => "Waiting",
            Self::Completed => "Completed",
            Self::Resolved => "Resolved",
            Self::Failed => "Could not finish",
            Self::Cancelled => "Cancelled",
            Self::Interrupted => "Interrupted",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ApprovalRef {
    Automation {
        run: String,
        question: String,
        digest: String,
    },
    Task(String),
    Conversation(u64),
}

#[derive(Clone, Debug, Serialize)]
pub struct Question {
    pub files: Vec<ArtifactRef>,
    pub request: Option<ApprovalRequest>,
    pub id: ApprovalRef,
    pub task: Option<String>,
    pub title: String,
    pub detail: String,
    pub choices: Vec<Choice>,
}

pub struct ConversationQuestion {
    pub id: u64,
    pub session: String,
    pub request: ApprovalRequest,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkView {
    pub delivery_error: Option<String>,
    pub run: Option<String>,
    pub id: String,
    pub project: String,
    pub title: String,
    pub target: Option<ark_orchestrate::WorkTarget>,
    pub start_receipt: Option<String>,
    pub conversation: Option<String>,
    pub conversation_missing: bool,
    pub root_task: Option<String>,
    pub tasks: Vec<String>,
    pub state: State,
    pub status: String,
    pub reason: Option<String>,
    pub questions: Vec<Question>,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub failures: Vec<ark_orchestrate::WorkFailure>,
    pub outputs: Vec<ArtifactRef>,
    pub origin: String,
    pub updated_ms: u64,
    pub cancel_tasks: Vec<String>,
    pub cancel_conversation: bool,
    pub retry_task: Option<String>,
}

#[derive(Serialize)]
pub struct Page {
    pub items: Vec<WorkView>,
    pub next: Option<String>,
}

pub fn retained_answer(entry: &WorkEntry) -> Option<String> {
    let followup = !entry.general.is_empty() || entry.tasks.iter().any(|task| task.run.is_none());
    if let Some(run) = &entry.run {
        if !followup && !run.output.text.is_empty() {
            return Some(run.output.text.clone());
        }
    }
    let task = entry
        .tasks
        .iter()
        .filter(|task| task.result.is_some() && (entry.run.is_none() || task.run.is_none()))
        .max_by_key(|task| (task.finished_ms, task.created_ms));
    let general = entry
        .general
        .iter()
        .filter_map(|(_, _, result)| result.as_ref())
        .filter(|result| result.answer.is_some())
        .max_by_key(|result| result.at_ms);
    match (task, general) {
        (Some(task), Some(general)) if general.at_ms > task.finished_ms => general.answer.clone(),
        (Some(task), _) => task.result.clone(),
        (None, Some(general)) => general.answer.clone(),
        _ => None,
    }
}

pub fn project(page: WorkPage, labels: &Labels, conversations: &[ConversationQuestion]) -> Page {
    Page {
        items: page
            .entries
            .into_iter()
            .map(|entry| view(entry, labels, conversations))
            .collect(),
        next: page.next,
    }
}

fn view(entry: WorkEntry, labels: &Labels, conversations: &[ConversationQuestion]) -> WorkView {
    let mut questions: Vec<_> = entry
        .tasks
        .iter()
        .filter(|task| !task.state.is_final())
        .filter_map(|task| {
            task.question
                .as_ref()
                .filter(|question| question.open())
                .map(|question| (task, question))
        })
        .map(|(task, question)| Question {
            files: vec![],
            request: Some(question.request.clone()),
            id: ApprovalRef::Task(question.id.clone()),
            task: Some(task.id.clone()),
            title: approval_title(&question.request),
            detail: labels.approval_line(&question.request),
            choices: approval_choices(&question.request),
        })
        .collect();
    questions.extend(
        conversations
            .iter()
            .filter(|question| {
                Some(&question.session) == entry.item.conversation.as_ref()
                    && question.request.task.is_none()
            })
            .map(|question| Question {
                files: vec![],
                request: Some(question.request.clone()),
                id: ApprovalRef::Conversation(question.id),
                task: None,
                title: approval_title(&question.request),
                detail: labels.approval_line(&question.request),
                choices: approval_choices(&question.request),
            }),
    );
    if let Some(run) = &entry.run {
        if run.phase == ark_orchestrate::automation::RunPhase::WaitingApproval
            && !run.cancel_requested
        {
            for question in run
                .steps
                .iter()
                .filter_map(|step| step.question.as_ref())
                .chain(run.questions.values())
                .filter(|question| question.decision.is_none())
            {
                questions.push(Question {
                    files: question.files.clone(),
                    request: question.action.clone(),
                    id: ApprovalRef::Automation {
                        run: run.id.clone(),
                        question: question.id.clone(),
                        digest: question.digest.clone(),
                    },
                    task: question
                        .action
                        .as_ref()
                        .and_then(|request| request.task.clone()),
                    title: question.title.clone(),
                    detail: question.detail.clone(),
                    choices: vec![
                        Choice {
                            label: "Approve".into(),
                            decision: ark_sdk::prelude::ApprovalDecision::AllowOnce,
                            hint: None,
                        },
                        Choice {
                            label: "Deny".into(),
                            decision: ark_sdk::prelude::ApprovalDecision::Deny,
                            hint: None,
                        },
                    ],
                });
            }
        }
    }
    let general_has = |wanted| entry.general.iter().any(|(_, state, _)| *state == wanted);
    let running = entry
        .tasks
        .iter()
        .any(|task| task.state == TaskState::Running)
        || general_has(GeneralWorkState::Running)
        || general_has(GeneralWorkState::Starting);
    let open = entry.tasks.iter().any(|task| !task.state.is_final())
        || general_has(GeneralWorkState::Pending);
    let unresolved: Vec<_> = entry
        .failures
        .iter()
        .filter(|failure| !failure.resolved)
        .collect();
    let failed =
        unresolved.iter().any(|failure| !failure.interrupted) || !entry.missing_tasks.is_empty();
    let interrupted = unresolved.iter().any(|failure| failure.interrupted);
    let completed = entry.tasks.iter().any(|task| {
        task.state == TaskState::Done
            && (task
                .result
                .as_ref()
                .is_some_and(|answer| !answer.trim().is_empty())
                || !task.outputs.is_empty())
    }) || entry.general.iter().any(|(_, state, result)| {
        *state == GeneralWorkState::Completed
            && result.as_ref().is_some_and(|result| {
                result
                    .answer
                    .as_ref()
                    .is_some_and(|answer| !answer.trim().is_empty())
                    || !result.outputs.is_empty()
            })
    });
    let mut state = if !questions.is_empty() {
        State::Attention
    } else if running {
        State::InProgress
    } else if open {
        State::Waiting
    } else if failed {
        State::Failed
    } else if interrupted {
        State::Interrupted
    } else if completed {
        State::Completed
    } else if entry
        .tasks
        .iter()
        .any(|task| task.state == TaskState::Cancelled)
        || general_has(GeneralWorkState::Cancelled)
    {
        State::Cancelled
    } else if !entry.failures.is_empty() && entry.failures.iter().all(|failure| failure.resolved) {
        State::Resolved
    } else if general_has(GeneralWorkState::Deleted) {
        State::Failed
    } else {
        State::Waiting
    };
    if let Some(run) = &entry.run {
        use ark_orchestrate::automation::RunPhase;
        if questions.is_empty()
            && entry.general.is_empty()
            && entry.tasks.iter().all(|task| task.run.is_some())
        {
            state = match run.phase {
                RunPhase::Pending => State::Waiting,
                RunPhase::Running => State::InProgress,
                RunPhase::WaitingApproval => State::Attention,
                RunPhase::Completed => State::Completed,
                RunPhase::Failed if unresolved.is_empty() => State::Resolved,
                RunPhase::Failed => State::Failed,
                RunPhase::Interrupted => State::Interrupted,
                RunPhase::Cancelled => State::Cancelled,
            };
        }
    }
    let summary = retained_answer(&entry).map(|answer| clip(&answer, 600));
    let error = unresolved
        .last()
        .map(|failure| clip(&failure.error, 500))
        .or_else(|| {
            (!entry.missing_tasks.is_empty()).then(|| "Some work records are unavailable".into())
        })
        .or_else(|| {
            general_has(GeneralWorkState::Deleted).then(|| "The conversation was deleted".into())
        });
    let mut output_ids = HashSet::new();
    let outputs = entry
        .tasks
        .iter()
        .flat_map(|task| task.outputs.iter())
        .chain(
            entry
                .general
                .iter()
                .filter_map(|(_, _, result)| result.as_ref())
                .flat_map(|result| result.outputs.iter()),
        )
        .chain(
            entry
                .run
                .iter()
                .flat_map(|run| run.steps.iter())
                .filter_map(|step| step.output.as_ref())
                .flat_map(|output| output.files.iter()),
        )
        .filter(|artifact| output_ids.insert(artifact.id.clone()))
        .cloned()
        .collect();
    let reason = match state {
        State::Waiting
            if entry
                .tasks
                .iter()
                .any(|task| task.state == TaskState::Suspended) =>
        {
            Some("Paused".into())
        }
        State::Waiting
            if entry
                .tasks
                .iter()
                .any(|task| matches!(task.state, TaskState::Pending | TaskState::Waiting)) =>
        {
            Some("Waiting for an earlier step".into())
        }
        State::Waiting => Some("Waiting to start".into()),
        State::Interrupted => Some("Open the conversation to continue".into()),
        _ => None,
    };
    let retry_task = entry
        .tasks
        .iter()
        .rev()
        .find(|task| {
            task.run.is_none()
                && matches!(task.state, TaskState::Failed | TaskState::Cancelled)
                && task.result.is_none()
        })
        .map(|task| task.id.clone());
    let cancel_conversation =
        general_has(GeneralWorkState::Running) || general_has(GeneralWorkState::Starting);
    WorkView {
        delivery_error: entry.delivery_error,
        run: entry.item.run.clone(),
        target: entry.target,
        start_receipt: entry.start_receipt,
        id: entry.item.id,
        project: entry.item.project,
        title: entry.item.title,
        conversation: entry.item.conversation,
        conversation_missing: entry.conversation_missing,
        root_task: entry.item.tasks.first().cloned(),
        tasks: entry.tasks.iter().map(|task| task.id.clone()).collect(),
        state,
        status: state.label().into(),
        reason,
        questions,
        summary,
        error,
        failures: entry.failures,
        outputs,
        origin: if let Some(run) = &entry.run {
            match run.origin {
                ark_orchestrate::automation::RunOrigin::Manual { .. }
                | ark_orchestrate::automation::RunOrigin::ScheduleManual { .. } => "manual",
                ark_orchestrate::automation::RunOrigin::Schedule { .. } => "schedule",
                ark_orchestrate::automation::RunOrigin::Webhook { .. } => "webhook",
            }
            .into()
        } else if entry.tasks.iter().any(|task| task.schedule.is_some()) {
            "schedule".into()
        } else {
            "manual".into()
        },
        updated_ms: entry
            .run
            .as_ref()
            .map(|run| run.updated_ms.max(entry.item.updated_ms))
            .unwrap_or(entry.item.updated_ms),
        cancel_tasks: entry
            .tasks
            .iter()
            .filter(|task| !task.state.is_final())
            .map(|task| task.id.clone())
            .collect(),
        cancel_conversation: cancel_conversation
            || entry.run.as_ref().is_some_and(|run| !run.phase.is_final()),
        retry_task,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_orchestrate::{AgentSpec, Models, Orchestrator, TaskSpec, WorkTarget};
    use ark_sdk::prelude::ark_model::{self, Completion, Message, Provider, ToolDef};
    use ark_sdk::Ark;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct Alternating(AtomicUsize);
    #[async_trait::async_trait]
    impl Provider for Alternating {
        fn describe(&self) -> String {
            "synthetic work status provider".into()
        }
        fn model_id(&self) -> String {
            "test".into()
        }
        async fn complete(
            &self,
            _system: &str,
            _messages: &[Message],
            _tools: &[ToolDef],
        ) -> ark_model::Result<Completion> {
            let call = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            if call % 2 == 1 {
                return Err(ark_model::ProviderError::Api {
                    provider: "test".into(),
                    detail: format!("synthetic failure {call}"),
                });
            }
            Ok(Completion {
                content: "synthetic successful result".into(),
                tool_calls: vec![],
                usage: Default::default(),
            })
        }
    }

    #[tokio::test]
    async fn unresolved_failure_precedes_a_later_success_and_resolution_has_an_exact_scope() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(Alternating(AtomicUsize::new(0)));
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(provider.clone())
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project_record = ark.create_project("project", "test").unwrap();
        let models = Arc::new(
            Models::builder()
                .default_model("test")
                .provider("test", provider)
                .build(),
        );
        let o = Orchestrator::builder(&ark, &project_record)
            .models(models)
            .build()
            .unwrap();
        o.spawn(AgentSpec::new("reader")).await.unwrap();
        let target = WorkTarget::Agent {
            name: "reader".into(),
        };
        let failed = o
            .admit_work(target.clone(), TaskSpec::new("first"), "desktop", "one")
            .await
            .unwrap();
        o.execute_work(&failed.id).unwrap();
        o.wait(failed.task.as_deref().unwrap()).await.unwrap();
        let pending = project(
            o.work_detail("a", &failed.work).unwrap(),
            &Labels::default(),
            &[],
        );
        let first_failure = &pending.items[0].failures[0];
        o.resolve_work_failure(
            "a",
            &failed.work,
            first_failure.run.clone(),
            &first_failure.digest,
            true,
        )
        .unwrap();
        assert_eq!(
            project(
                o.work_detail("a", &failed.work).unwrap(),
                &Labels::default(),
                &[]
            )
            .items[0]
                .state,
            State::Resolved
        );
        o.resolve_work_failure(
            "a",
            &failed.work,
            first_failure.run.clone(),
            &first_failure.digest,
            false,
        )
        .unwrap();
        let success = o
            .admit_work(
                target,
                TaskSpec::new("second").item(&failed.work),
                "desktop",
                "two",
            )
            .await
            .unwrap();
        o.execute_work(&success.id).unwrap();
        o.wait(success.task.as_deref().unwrap()).await.unwrap();
        let page = project(
            o.work_page("a", false, None, 10).unwrap(),
            &Labels::default(),
            &[],
        );
        assert_eq!(page.items[0].state, State::Failed);
        assert_eq!(
            page.items[0].summary.as_deref(),
            Some("synthetic successful result")
        );
        let failure = page.items[0].failures[0].clone();
        assert!(o
            .resolve_work_failure("a", &success.work, failure.run.clone(), "v1:stale", true)
            .is_err());
        assert!(o
            .resolve_work_failure(
                "a",
                "another-work",
                failure.run.clone(),
                &failure.digest,
                true
            )
            .is_err());
        o.resolve_work_failure(
            "a",
            &failed.work,
            failure.run.clone(),
            &failure.digest,
            true,
        )
        .unwrap();
        assert_eq!(
            project(
                o.work_page("a", false, None, 10).unwrap(),
                &Labels::default(),
                &[]
            )
            .items[0]
                .state,
            State::Completed
        );
        assert_eq!(
            project(
                o.work_page("b", false, None, 10).unwrap(),
                &Labels::default(),
                &[]
            )
            .items[0]
                .state,
            State::Failed
        );
        o.resolve_work_failure(
            "a",
            &failed.work,
            failure.run.clone(),
            &failure.digest,
            false,
        )
        .unwrap();
        assert_eq!(
            project(
                o.work_page("a", false, None, 10).unwrap(),
                &Labels::default(),
                &[]
            )
            .items[0]
                .state,
            State::Failed
        );
        o.resolve_work_failure(
            "a",
            &failed.work,
            failure.run.clone(),
            &failure.digest,
            true,
        )
        .unwrap();
        o.retry(failed.task.as_deref().unwrap()).unwrap();
        o.wait(failed.task.as_deref().unwrap()).await.unwrap();
        assert!(o
            .resolve_work_failure("a", &failed.work, failure.run, &failure.digest, true)
            .is_err());
        assert_eq!(
            project(
                o.work_page("a", false, None, 10).unwrap(),
                &Labels::default(),
                &[]
            )
            .items[0]
                .state,
            State::Failed
        );
    }
}
