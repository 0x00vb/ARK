use super::*;
use ark_interact::ark_orchestrate::{OrchError, WorkReceipt, WorkTarget};

struct ClientWork {
    orchestrator: Orchestrator,
    target: WorkTarget,
    previous: WorkReceipt,
}

impl Host {
    pub(crate) fn client_internal_execution_sessions(
        &self,
    ) -> anyhow::Result<std::collections::HashSet<String>> {
        Ok(
            ark_sdk::prelude::ark_runtime::session::Session::list(&self.engine, None)?
                .into_iter()
                .filter(|session| session.execution_managed)
                .map(|session| session.id.to_string())
                .collect(),
        )
    }

    fn client_work(&self, conversation: &str) -> anyhow::Result<Option<ClientWork>> {
        let Some(header) = self.engine.store.get_session(conversation)? else {
            return Ok(None);
        };
        let header: ark_sdk::prelude::ark_runtime::session::SessionRecord =
            serde_json::from_value(header)?;
        let Some(work) = header.work_item else {
            return Ok(None);
        };
        let orchestrator = self.siblings.project_orchestrator(&header.project)?;
        let mut detail =
            orchestrator.work_detail(ark_interact::ark_orchestrate::board::PERSON, &work)?;
        let entry = detail
            .entries
            .pop()
            .ok_or_else(|| anyhow::anyhow!("the conversation's work is unavailable"))?;
        let Some(previous) = entry
            .receipts
            .iter()
            .filter(|receipt| receipt.conversation == conversation)
            .max_by_key(|receipt| receipt.sequence)
            .cloned()
        else {
            return Ok(None);
        };
        let target = entry
            .targets
            .get(&previous.id)
            .ok_or_else(|| anyhow::anyhow!("the conversation's admitted target is unavailable"))?
            .clone();
        anyhow::ensure!(
            previous.project == header.project.as_str() && previous.work == work,
            "the conversation's work belongs to another project"
        );
        Ok(Some(ClientWork {
            orchestrator,
            target,
            previous,
        }))
    }

    pub(super) fn client_has_work_binding(&self, conversation: &str) -> anyhow::Result<bool> {
        Ok(self.client_work(conversation)?.is_some())
    }

    pub(super) async fn apply_client_work_send(
        self: &Arc<Self>,
        record_id: &str,
        device: &str,
        request: &ark_protocol::MutationRequest,
        record: &mut Value,
    ) -> Result<Option<(&'static str, &'static str)>, ClientMutationError> {
        let Some(work) = self
            .client_work(&request.target)
            .map_err(ClientMutationError::Store)?
        else {
            return Ok(None);
        };
        let request_key = format!("native:{record_id}");
        let receipt = match work
            .orchestrator
            .work_receipt("manual", &request_key)
            .map_err(|error| ClientMutationError::Store(error.into()))?
        {
            Some(receipt) => receipt,
            None => match work
                .orchestrator
                .try_admit_work_followup(
                    work.target,
                    TaskSpec::new(request.input["body"].as_str().unwrap_or_default())
                        .item(work.previous.work.clone()),
                    "manual",
                    &request_key,
                    &work.previous.id,
                )
                .await
            {
                Ok(Some(receipt)) => receipt,
                Ok(None) => return Ok(Some(("accepted", "Accepted"))),
                Err(
                    OrchError::Terminated(_)
                    | OrchError::UnknownAgent(_)
                    | OrchError::NotAllowed(_),
                ) => {
                    return Ok(Some(("no_longer_applicable", "Nothing changed")));
                }
                Err(error) => return Err(ClientMutationError::Store(error.into())),
            },
        };
        if receipt.conversation != request.target
            || receipt.work != work.previous.work
            || receipt.project != work.previous.project
        {
            return Err(ClientMutationError::CounterConflict);
        }
        record["effect"] = json!({"workReceipt":receipt.id,"conversation":receipt.conversation});
        self.engine
            .store
            .put_record("client_mutation", record_id, device, record)
            .map_err(|error| ClientMutationError::Store(error.into()))?;
        if let Some(task_id) = &receipt.task {
            let task: ark_interact::ark_orchestrate::TaskRecord = serde_json::from_value(
                self.engine
                    .store
                    .get_record("orch_task", task_id)
                    .map_err(|error| ClientMutationError::Store(error.into()))?
                    .ok_or_else(|| {
                        ClientMutationError::Store(anyhow::anyhow!(
                            "the admitted task is unavailable"
                        ))
                    })?,
            )
            .map_err(|error| ClientMutationError::Store(error.into()))?;
            if task.submission.as_deref() != Some(receipt.id.as_str())
                || task.conversation.as_deref() != Some(receipt.conversation.as_str())
                || task.item.as_deref() != Some(receipt.work.as_str())
            {
                return Err(ClientMutationError::CounterConflict);
            }
            if !task.state.is_final() {
                work.orchestrator
                    .execute_work(&receipt.id)
                    .map_err(|error| ClientMutationError::Store(error.into()))?;
            }
        } else if work
            .orchestrator
            .general_work_state(&receipt.id)
            .map_err(|error| ClientMutationError::Store(error.into()))?
            == ark_interact::ark_orchestrate::GeneralWorkState::Pending
        {
            let conv = self
                .chat_conv(&receipt.conversation, "", true)
                .await
                .map_err(ClientMutationError::Store)?;
            if !conv.is_running() {
                self.start_turn_on(
                    conv,
                    work.orchestrator
                        .general_work_input(&receipt.id)
                        .map_err(|error| ClientMutationError::Store(error.into()))?,
                    true,
                )
                .map_err(ClientMutationError::Store)?;
            }
        }
        Ok(Some(("completed", "Sent")))
    }
}
