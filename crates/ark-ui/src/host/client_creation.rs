use super::*;

impl Host {
    pub(super) fn valid_client_creation(&self, request: &ark_protocol::MutationRequest) -> bool {
        let name = request.input["name"].as_str().unwrap_or_default();
        let clean =
            !name.trim().is_empty() && name == name.trim() && !name.chars().any(char::is_control);
        let keys = request.input.as_object().map(|v| v.len()).unwrap_or(0);
        match request.operation.as_str() {
            "project.create" => {
                clean && name.chars().count() <= 120 && request.target.is_empty() && keys == 1
            }
            "agent.create" => {
                clean
                    && name.len() <= 40
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    && request.input["role"].as_str().is_some_and(|role| {
                        role.chars().count() <= 120 && !role.chars().any(char::is_control)
                    })
                    && optional_text(&request.input["title"], 120)
                    && optional_text(&request.input["look"], 64)
                    && keys
                        == 2 + ["title", "look"]
                            .iter()
                            .filter(|k| request.input.get(**k).is_some())
                            .count()
            }
            _ => false,
        }
    }

    pub(super) async fn apply_client_creation(
        self: &Arc<Self>,
        key: &str,
        request: &ark_protocol::MutationRequest,
    ) -> Result<(&'static str, &'static str, Option<String>), ClientMutationError> {
        let name = request.input["name"]
            .as_str()
            .ok_or(ClientMutationError::Unsupported)?;
        if request.operation == "project.create" {
            let id = ProjectId(format!("client-{:x}", Sha256::digest(key.as_bytes())));
            if self
                .engine
                .store
                .get_project(&id)
                .map_err(|e| ClientMutationError::Store(e.into()))?
                .is_none()
            {
                let mut project = Project::new(name, "");
                project.id = id.clone();
                match self.engine.store.put_project(&project) {
                    Ok(()) => self.emit(json!({ "kind": "summary", "summary": self.summary() })),
                    Err(ark_store::StoreError::ProjectNameConflict) => {
                        return Ok(("refused", "A project with that name already exists", None))
                    }
                    Err(error) => return Err(ClientMutationError::Store(error.into())),
                }
            }
            return Ok(("completed", "Project created", Some(id.to_string())));
        }
        let Some(project) = self
            .engine
            .store
            .get_project(&ProjectId(request.target.clone()))
            .map_err(|e| ClientMutationError::Store(e.into()))?
            .filter(|p| p.parent.is_none())
        else {
            return Ok((
                "no_longer_applicable",
                "That project is no longer available",
                None,
            ));
        };
        let orchestrator = self
            .siblings
            .project_orchestrator(&project.id)
            .map_err(ClientMutationError::Store)?;
        let spec: AgentSpec = serde_json::from_value(json!({
            "name": name, "role": request.input["role"], "tools": ["*"],
            "title": request.input["title"].as_str().unwrap_or_default(),
            "look": request.input["look"].as_str().unwrap_or_default(),
            "allow_mutate": false, "delegates_to": [], "model": null,
        }))
        .map_err(|_| ClientMutationError::Unsupported)?;
        match orchestrator.create_once(spec, key).await {
            Ok(agent) => {
                self.emit(json!({ "kind": "sessions", "session": format!("agent:{}:{}", project.id, agent.name) }));
                Ok((
                    "completed",
                    "Agent created",
                    Some(format!("agent:{}:{}", project.id, agent.name)),
                ))
            }
            Err(ark_interact::ark_orchestrate::OrchError::NotAllowed(_)) => {
                Ok(("refused", "An agent with that name already exists", None))
            }
            Err(error) => Err(ClientMutationError::Store(error.into())),
        }
    }
}

/// Absent, or a string of at most `max` characters with no control characters.
fn optional_text(value: &Value, max: usize) -> bool {
    value.is_null()
        || value
            .as_str()
            .is_some_and(|v| v.chars().count() <= max && !v.chars().any(char::is_control))
}
