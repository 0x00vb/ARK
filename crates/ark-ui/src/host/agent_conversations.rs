use super::*;
use ark_interact::ark_orchestrate::{
    agent_threads, latest_thread_preview, person_tasks, AgentRecord, AgentState, AgentThread,
    OrchError, Origin, TaskRecord, TaskSpec, TaskState,
};
use ark_sdk::prelude::ark_runtime::session::SessionRecord;
use std::collections::{HashMap, HashSet};

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

/// A project id or an agent name as the client routes carry it.
pub(crate) fn valid_segment(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The conversation `id` names within `entry`: `None` for the default one.
pub(crate) fn thread_of_chat<'a>(entry: &ClientAgent, id: &'a str) -> Option<&'a str> {
    (!id.starts_with("agent:") && entry.agent.session.as_deref() != Some(id)).then_some(id)
}

fn address(id: &str) -> anyhow::Result<Option<(&str, &str)>> {
    let Some(value) = id.strip_prefix("agent:") else {
        return Ok(None);
    };
    let Some((project, name)) = value.split_once(':') else {
        return Err(AgentUnavailable.into());
    };
    if !valid_segment(project, 128) || !valid_segment(name, 64) {
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
                // One damaged record must not hide every other agent.
                match self.checked_agent(&project, value) {
                    Ok(agent) => agents.push(ClientAgent {
                        project: project.clone(),
                        agent,
                    }),
                    Err(error) => {
                        tracing::warn!(project = %project.id, %error, "skipping an agent record")
                    }
                }
            }
        }
        Ok(agents)
    }

    fn checked_agent(
        &self,
        project: &ark_core::Project,
        value: serde_json::Value,
    ) -> anyhow::Result<AgentRecord> {
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
        Ok(agent)
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

    pub(crate) async fn client_agent_model(
        &self,
        entry: &ClientAgent,
    ) -> ark_protocol::ConversationModel {
        let providers = client_model_providers(ark_model::catalog().await);
        let spec = entry
            .agent
            .spec
            .model
            .as_deref()
            .unwrap_or(&entry.agent.model);
        let selection = selection_for_canonical(&providers, spec);
        let display = selection
            .as_ref()
            .and_then(|s| selection_display(&providers, s))
            .unwrap_or_else(|| entry.agent.model.clone());
        ark_protocol::ConversationModel {
            selection,
            display,
            can_select: entry.agent.state == AgentState::Idle,
        }
    }

    pub(crate) async fn apply_client_agent_model(
        &self,
        entry: &ClientAgent,
        selection: &ark_protocol::ModelSelection,
        request: &str,
    ) -> Result<(&'static str, &'static str), ClientMutationError> {
        let providers = client_model_providers(ark_model::catalog().await);
        let Some(spec) = canonical_selection(&providers, selection) else {
            return Ok(("refused", "That model is no longer available"));
        };
        let orchestrator = self
            .siblings
            .project_orchestrator(&entry.project.id)
            .map_err(ClientMutationError::Store)?;
        match orchestrator
            .select_idle_model(&entry.agent.name, &spec, request)
            .await
        {
            Ok(_) => {
                self.emit(json!({ "kind": "sessions", "session": entry.id() }));
                Ok(("completed", "Model changed"))
            }
            Err(OrchError::NotAllowed(reason)) if reason == "the agent is busy" => {
                Ok(("refused", "Wait for this agent to finish"))
            }
            Err(OrchError::UnknownAgent(_)) => {
                Ok(("no_longer_applicable", "That agent is no longer available"))
            }
            Err(error) => Err(ClientMutationError::Store(error.into())),
        }
    }

    pub(crate) fn client_agent_schedules(
        &self,
        entry: &ClientAgent,
    ) -> anyhow::Result<Vec<ark_protocol::AgentScheduleSummary>> {
        let orchestrator = self.siblings.project_orchestrator(&entry.project.id)?;
        Ok(ark_interact::orchestration::schedules(&orchestrator, None)
            .into_iter()
            .filter(|s| s.agent == entry.agent.name)
            .map(|s| ark_protocol::AgentScheduleSummary {
                id: s.id,
                title: s.objective,
                when: s.says,
                next: s.next,
                state: s.status,
            })
            .collect())
    }

    pub(crate) fn client_agent_tasks(
        &self,
        entry: &ClientAgent,
        thread: Option<&str>,
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
            let in_conversation = match thread {
                Some(thread) => task.thread.as_deref() == Some(thread),
                None => task
                    .thread
                    .as_ref()
                    .is_none_or(|t| entry.agent.session.as_ref() == Some(t)),
            };
            if task.owner == entry.agent.name && task.conversation.is_none() && in_conversation {
                tasks.push(task);
            }
        }
        tasks.sort_by(|a, b| (a.created_ms, &a.id).cmp(&(b.created_ms, &b.id)));
        Ok(tasks)
    }

    pub(crate) async fn apply_client_conversation_start(
        self: &Arc<Self>,
        record_id: &str,
        device: &str,
        request: &ark_protocol::MutationRequest,
        record: &mut Value,
    ) -> Result<(&'static str, &'static str, Option<String>), ClientMutationError> {
        if let Some(id) = record["effect"]["conversation"].as_str() {
            return Ok(("completed", "Started", Some(id.to_string())));
        }
        let project = request.input["project"].as_str().unwrap_or_default();
        let name = request.input["agent"].as_str().unwrap_or_default();
        let Some(entry) = self
            .client_agents()
            .map_err(ClientMutationError::Store)?
            .into_iter()
            .find(|e| e.project.id.as_str() == project && e.agent.name == name)
        else {
            return Ok((
                "no_longer_applicable",
                "That agent is no longer available",
                None,
            ));
        };
        let orchestrator = self
            .siblings
            .project_orchestrator(&entry.project.id)
            .map_err(ClientMutationError::Store)?;
        match orchestrator.start_thread(name) {
            Ok(id) => {
                record["effect"] = json!({ "conversation": id });
                self.engine
                    .store
                    .put_record("client_mutation", record_id, device, record)
                    .map_err(|error| ClientMutationError::Store(error.into()))?;
                Ok(("completed", "Started", Some(id)))
            }
            Err(OrchError::Terminated(_) | OrchError::UnknownAgent(_)) => Ok((
                "no_longer_applicable",
                "That agent is no longer available",
                None,
            )),
            Err(OrchError::NotAllowed(_)) => {
                Ok(("refused", "That agent keeps one conversation", None))
            }
            Err(error) => Err(ClientMutationError::Store(error.into())),
        }
    }

    /// The conversations of the agent `project`/`name`, or `None` when there is no such agent.
    pub(crate) fn client_agent_conversations(
        &self,
        project: &str,
        name: &str,
    ) -> anyhow::Result<Option<Vec<AgentThread>>> {
        let known = self.client_agents()?.into_iter().any(|e| {
            e.project.id.as_str() == project
                && e.agent.name == name
                && e.agent.state != AgentState::Terminated
        });
        if !known {
            return Ok(None);
        }
        Ok(Some(agent_threads(&self.engine, project, name)?))
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
        let thread = thread_of_chat(&entry, &request.target);
        let mut spec = TaskSpec::new(body);
        spec.person = true;
        if let Some(thread) = thread {
            let running = agent_threads(&self.engine, entry.project.id.as_str(), &entry.agent.name)
                .map_err(|error| ClientMutationError::Store(error.into()))?
                .into_iter()
                .any(|row| row.id == thread && row.running.is_some());
            if running {
                return match orchestrator.message_thread(
                    "user",
                    &entry.agent.name,
                    Some(thread),
                    body,
                    "",
                ) {
                    Ok(_) => {
                        record["effect"] = json!({ "steered": thread, "conversation": entry.id() });
                        self.engine
                            .store
                            .put_record("client_mutation", record_id, device, record)
                            .map_err(|error| ClientMutationError::Store(error.into()))?;
                        Ok(("completed", "Sent"))
                    }
                    Err(OrchError::Terminated(_) | OrchError::UnknownAgent(_)) => {
                        Ok(("no_longer_applicable", "Nothing changed"))
                    }
                    Err(OrchError::NotAllowed(_)) => {
                        Ok(("refused", "That conversation cannot be written to"))
                    }
                    Err(error) => Err(ClientMutationError::Store(error.into())),
                };
            }
            spec = spec.thread(thread);
        }
        let task = match orchestrator
            .submit_once(&entry.agent.name, spec, record_id)
            .await
        {
            Ok(task) => task,
            Err(OrchError::Terminated(_) | OrchError::UnknownAgent(_)) => {
                return Ok(("no_longer_applicable", "Nothing changed"))
            }
            Err(OrchError::NotAllowed(_)) if thread.is_some() => {
                return Ok(("refused", "That conversation cannot be written to"))
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
            .client_agent_tasks(&entry, thread_of_chat(&entry, id))
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

/// The optional `thread` of a call: one of the agent's conversations.
pub(crate) fn thread_of(input: &Value) -> anyhow::Result<Option<String>> {
    match &input["thread"] {
        Value::Null => Ok(None),
        Value::String(t) if !t.is_empty() && t.len() <= 128 => Ok(Some(t.clone())),
        _ => anyhow::bail!("a conversation is named by its id"),
    }
}

/// A session that is a person's own chat: not Work's, not an automation's.
fn general(record: &SessionRecord) -> bool {
    record.work_item.is_none()
        && !record.execution_managed
        && record.execution_scope.is_none()
        && !record.background
}

impl Host {
    /// Every agent of every top-level project that has not ended, read from the store: no
    /// orchestrator is opened for them. Most recently changed first.
    pub(crate) fn all_agents(&self) -> anyhow::Result<Value> {
        let mut rows: Vec<(u64, Value)> = Vec::new();
        // The agents that ask the person something, per project: (project, agent).
        let mut asking_in: HashMap<String, HashSet<String>> = HashMap::new();
        let mut tasks_in: HashMap<String, Vec<TaskRecord>> = HashMap::new();
        for entry in self.client_agents()? {
            let agent = &entry.agent;
            if agent.state == AgentState::Terminated {
                continue;
            }
            let project = entry.project.id.as_str();
            if !asking_in.contains_key(project) {
                asking_in.insert(project.to_string(), self.asking_agents(project)?);
            }
            let asking = asking_in[project].contains(&agent.name);
            if !tasks_in.contains_key(project) {
                tasks_in.insert(project.to_string(), person_tasks(&self.engine, project)?);
            }
            let preview = latest_thread_preview(&self.engine, agent, &tasks_in[project])?;
            let own_folder = self
                .engine
                .store
                .get_project(&ProjectId(agent.scope.clone()))?
                .and_then(|p| p.workspace);
            rows.push((
                agent.updated_ms,
                json!({
                    "project": entry.project.id,
                    "project_name": entry.project.name,
                    "name": agent.name,
                    "title": agent.spec.title,
                    "look": agent.spec.look,
                    "role": agent.spec.role,
                    "state": agent.state,
                    "asking": asking,
                    "running": agent.running,
                    "updated_ms": agent.updated_ms,
                    "preview": preview,
                    "program": agent.on_program(),
                    "model": agent.spec.model,
                    "description": agent.spec.description,
                    "folder": own_folder.as_ref().or(entry.project.workspace.as_ref()),
                    "own_folder": own_folder.is_some(),
                }),
            ));
        }
        rows.sort_by_key(|row| std::cmp::Reverse(row.0));
        Ok(Value::Array(rows.into_iter().map(|(_, row)| row).collect()))
    }

    /// The agents of `project` with a task that waits for the person's answer. A task record
    /// that does not read is skipped, so one damaged record does not hide every agent.
    fn asking_agents(&self, project: &str) -> anyhow::Result<HashSet<String>> {
        let mut out = HashSet::new();
        for value in self.engine.store.list_records("orch_task", Some(project))? {
            match serde_json::from_value::<TaskRecord>(value) {
                Ok(task)
                    if !task.state.is_final()
                        && task.question.as_ref().is_some_and(|q| q.open()) =>
                {
                    out.insert(task.owner);
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, project, "skipped a task record that does not read")
                }
            }
        }
        Ok(out)
    }

    /// The conversations of `agent` in the top-level `project`, read from the store.
    pub(crate) fn project_agent_conversations(
        &self,
        project: &str,
        agent: &str,
    ) -> anyhow::Result<Value> {
        let top = self
            .engine
            .store
            .get_project(&ProjectId(project.to_string()))?
            .filter(|p| p.parent.is_none())
            .ok_or_else(|| anyhow::anyhow!("there is no project `{project}` here"))?;
        Ok(serde_json::to_value(agent_threads(
            &self.engine,
            top.id.as_str(),
            agent,
        )?)?)
    }

    /// The person's chats in every top-level project (no agent's, no Work's), newest first,
    /// as `sessions` lists them.
    pub(crate) fn general_sessions(&self) -> anyhow::Result<Value> {
        use ark_interact::interaction::project_sessions;
        let current = self.session_id();
        let working: Vec<String> = self
            .working()
            .iter()
            .filter_map(|w| w["session"].as_str().map(str::to_string))
            .collect();
        let mut rows = Vec::new();
        for project in self
            .engine
            .store
            .list_projects()?
            .into_iter()
            .filter(|p| p.parent.is_none())
        {
            let own: std::collections::HashSet<String> =
                Session::list(&self.engine, Some(&project.id))?
                    .into_iter()
                    .filter(general)
                    .map(|r| r.id.as_str().to_string())
                    .collect();
            rows.extend(
                project_sessions(&self.engine, project.id.as_str(), &current)?
                    .into_iter()
                    .filter(|row| own.contains(&row.id)),
            );
        }
        rows.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
        let mut v = serde_json::to_value(rows)?;
        for row in v.as_array_mut().into_iter().flatten() {
            if row["id"]
                .as_str()
                .is_some_and(|id| working.iter().any(|w| w == id))
            {
                row["working"] = json!(true);
            }
        }
        Ok(v)
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
