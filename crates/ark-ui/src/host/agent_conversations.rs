use super::*;
use ark_interact::ark_orchestrate::{
    AgentRecord, AgentState, OrchError, Origin, TaskRecord, TaskSpec, TaskState,
};

pub(crate) struct ClientAgent {
    pub project: Project,
    pub agent: AgentRecord,
}

impl ClientAgent {
    pub fn id(&self) -> String {
        format!("agent:{}:{}", self.project.id, self.agent.name)
    }
}

#[derive(Debug)]
pub(crate) struct AgentUnavailable;

impl std::fmt::Display for AgentUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("that agent conversation is unavailable")
    }
}
impl std::error::Error for AgentUnavailable {}

fn address(id: &str) -> anyhow::Result<Option<(&str, &str)>> {
    let Some(value) = id.strip_prefix("agent:") else {
        return Ok(None);
    };
    let Some((project, name)) = value.split_once(':') else {
        return Err(AgentUnavailable.into());
    };
    let valid = |text: &str, max: usize| {
        !text.is_empty()
            && text.len() <= max
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    if !valid(project, 128) || !valid(name, 64) {
        return Err(AgentUnavailable.into());
    }
    Ok(Some((project, name)))
}

impl Host {
    pub(crate) fn agent_conversations_enabled(&self) -> bool {
        self.orchs.supports_agents()
    }

    pub(crate) fn client_agents(&self) -> anyhow::Result<Vec<ClientAgent>> {
        let mut agents = Vec::new();
        for project in self
            .engine
            .store
            .list_projects()?
            .into_iter()
            .filter(|p| p.parent.is_none())
        {
            for value in self
                .engine
                .store
                .list_records("orch_agent", Some(project.id.as_str()))?
            {
                let agent: AgentRecord = serde_json::from_value(value)?;
                anyhow::ensure!(
                    agent.project == project.id.as_str(),
                    "agent belongs to another project"
                );
                let scope = self
                    .engine
                    .store
                    .get_project(&ProjectId(agent.scope.clone()))?
                    .ok_or_else(|| anyhow::anyhow!("agent context no longer exists"))?;
                anyhow::ensure!(
                    scope.parent.as_ref() == Some(&project.id),
                    "agent context belongs to another project"
                );
                agents.push(ClientAgent {
                    project: project.clone(),
                    agent,
                });
            }
        }
        Ok(agents)
    }

    pub(crate) fn client_agent(&self, id: &str) -> anyhow::Result<Option<ClientAgent>> {
        let requested = address(id)?;
        let session_scope = if requested.is_none() {
            self.engine
                .store
                .get_session(id)?
                .and_then(|s| s["project"].as_str().map(str::to_string))
        } else {
            None
        };
        for entry in self.client_agents()? {
            let matched = match requested {
                Some((project, name)) => {
                    entry.project.id.as_str() == project && entry.agent.name == name
                }
                None => {
                    entry.agent.session.as_deref() == Some(id)
                        || session_scope.as_deref() == Some(&entry.agent.scope)
                }
            };
            if matched {
                return Ok(Some(entry));
            }
        }
        if requested.is_some() {
            return Err(AgentUnavailable.into());
        }
        Ok(None)
    }

    pub(crate) fn client_agent_tasks(
        &self,
        entry: &ClientAgent,
    ) -> anyhow::Result<Vec<TaskRecord>> {
        let mut tasks = Vec::new();
        for value in self
            .engine
            .store
            .list_records("orch_task", Some(entry.project.id.as_str()))?
        {
            let task: TaskRecord = serde_json::from_value(value)?;
            anyhow::ensure!(
                task.project == entry.project.id.as_str(),
                "task belongs to another project"
            );
            if task.owner == entry.agent.name {
                tasks.push(task);
            }
        }
        tasks.sort_by(|a, b| (a.created_ms, &a.id).cmp(&(b.created_ms, &b.id)));
        Ok(tasks)
    }

    pub(crate) async fn apply_client_agent_send(
        self: &Arc<Self>,
        record_id: &str,
        device: &str,
        request: &ark_protocol::MutationRequest,
        record: &mut Value,
        entry: ClientAgent,
    ) -> Result<(&'static str, &'static str), ClientMutationError> {
        let orchestrator = self
            .siblings
            .project_orchestrator(&entry.project.id)
            .map_err(ClientMutationError::Store)?;
        if entry.agent.state == AgentState::Terminated {
            return Ok(("no_longer_applicable", "Nothing changed"));
        }
        let body = request.input["body"].as_str().unwrap_or_default();
        let task = match orchestrator
            .submit_once(&entry.agent.name, TaskSpec::new(body), record_id)
            .await
        {
            Ok(task) => task,
            Err(OrchError::Terminated(_) | OrchError::UnknownAgent(_)) => {
                return Ok(("no_longer_applicable", "Nothing changed"))
            }
            Err(error) => return Err(ClientMutationError::Store(error.into())),
        };
        record["effect"] = json!({ "task": task, "conversation": entry.id() });
        self.engine
            .store
            .put_record("client_mutation", record_id, device, record)
            .map_err(|error| ClientMutationError::Store(error.into()))?;
        Ok(("completed", "Sent"))
    }

    pub(crate) fn client_results(
        &self,
        id: &str,
    ) -> Result<Vec<ark_protocol::ResultSummary>, ClientResultError> {
        let Some(entry) = self.client_agent(id).map_err(ClientResultError::Store)? else {
            return self.client_session_results(id);
        };
        let mut summaries = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let tasks = self
            .client_agent_tasks(&entry)
            .map_err(ClientResultError::Store)?;
        for task in tasks.iter().rev() {
            for output in &task.outputs {
                if summaries.len() >= CLIENT_RESULT_COUNT {
                    return Ok(summaries);
                }
                if !seen.insert(&output.id) {
                    continue;
                }
                let Some(artifact) = self
                    .engine
                    .store
                    .get_artifact(
                        &ProjectId(entry.agent.scope.clone()),
                        &ArtifactId(output.id.clone()),
                    )
                    .map_err(|error| ClientResultError::Store(error.into()))?
                else {
                    continue;
                };
                if artifact.meta["before"] == true
                    || !matches!(artifact.provenance, Provenance::Produced { .. })
                {
                    continue;
                }
                summaries.push(ark_protocol::ResultSummary {
                    id: artifact.id.as_str().to_string(),
                    title: artifact.name,
                    project: entry.project.name.clone(),
                    created_at: artifact
                        .created_at
                        .format(&time::format_description::well_known::Rfc3339)
                        .map_err(|error| ClientResultError::Store(error.into()))?,
                    kind: artifact.kind.as_tag(),
                    byte_count: Some(artifact.size),
                    preview_text: None,
                    download_path: Some(format!(
                        "/client/v1/chats/{}/results/{}",
                        entry.id(),
                        artifact.id
                    )),
                });
            }
        }
        Ok(summaries)
    }

    pub(crate) fn client_result_file(
        &self,
        id: &str,
        result: &str,
        max_bytes: u64,
    ) -> Result<ClientResultFile, ClientResultError> {
        let Some(entry) = self.client_agent(id).map_err(ClientResultError::Store)? else {
            return self.client_session_result_file(id, result, max_bytes);
        };
        if !self
            .client_results(&entry.id())?
            .iter()
            .any(|artifact| artifact.id == result)
        {
            return Err(ClientResultError::ResultMissing);
        }
        self.client_artifact_file(
            &ProjectId(entry.agent.scope),
            &ArtifactId(result.to_string()),
            max_bytes,
        )
    }
}

pub(crate) fn task_sender(task: &TaskRecord) -> &str {
    match &task.origin {
        Origin::External => "You",
        Origin::Agent { agent, .. } => agent,
    }
}

pub(crate) fn task_display(task: &TaskRecord) -> &str {
    match task.state {
        TaskState::Pending | TaskState::Queued => "Waiting to start",
        TaskState::Running => "Working",
        TaskState::Waiting => "Waiting",
        TaskState::Suspended => "Paused",
        TaskState::Done => task.result.as_deref().unwrap_or("Done"),
        TaskState::Failed => task.error.as_deref().unwrap_or("Could not finish"),
        TaskState::Cancelled => "Stopped",
    }
}
