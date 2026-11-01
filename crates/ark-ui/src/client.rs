//! Adapter from ARK's shared interaction meanings to external client protocol v1.

use std::sync::Arc;

use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::host::agent_conversations::{task_display, task_sender, ClientAgent};
use crate::host::Host;
use crate::resource::{ResourceOperation, ResourceOperationKind};
use ark_interact::ark_orchestrate::{AgentState, TaskState};

pub(crate) async fn home(host: &Arc<Host>) -> anyhow::Result<Value> {
    let hello = host.call("hello", json!({})).await?;
    let attention = host.call("attention_all", json!({})).await?;
    let sessions = all_chats(host).await?;
    let working = host.call("working", json!({})).await?;

    let mut needs = Vec::new();
    let mut recent = Vec::new();
    for item in attention["items"].as_array().into_iter().flatten() {
        match item["kind"].as_str().unwrap_or("") {
            "question" => needs.push(attention_item(item)),
            "finished" | "failed" => recent.push(result_item(item)),
            _ => {}
        }
    }

    let mut active = working
        .as_array()
        .into_iter()
        .flatten()
        .map(working_item)
        .collect::<Vec<_>>();
    for entry in host.client_agents()? {
        for task in host
            .client_agent_tasks(&entry)?
            .iter()
            .filter(|task| !task.state.is_final() && task.state != TaskState::Suspended)
        {
            active.push(
                json!({ "id": task.id, "title": task.objective, "step": task_display(task),
                "teammate": entry.agent.name, "computer": "This computer", "elapsedSeconds": 0,
                "phase": "working", "chatId": entry.id() }),
            );
        }
    }
    let now = (OffsetDateTime::now_utc().unix_timestamp().max(0) as u64).saturating_mul(1000);
    for run in ark_interact::ark_orchestrate::automation_attention_runs(
        &host.engine_for_client().store,
        now.saturating_sub(ark_interact::attention::KEPT_MS),
    )? {
        if run.phase.is_final()
            || run.phase == ark_interact::ark_orchestrate::automation::RunPhase::Interrupted
        {
            continue;
        }
        let step = run.steps.iter().find(|step| {
            step.phase != ark_interact::ark_orchestrate::automation::StepPhase::Completed
        });
        active.push(json!({ "id":run.id,"title":run.revision.draft.name,"step":step.and_then(|current|run.revision.draft.steps.iter().find(|step|step.id==current.id)).map(|step|step.name.as_str()).unwrap_or("Working"),"teammate":"ARK","computer":"This computer","elapsedSeconds":now.saturating_sub(run.created_ms)/1000,"phase":"working","chatId":run.conversation }));
    }
    Ok(json!({
        "sequence": decimal(hello["seq"].as_u64().unwrap_or(0)),
        "attention": needs,
        "working": active,
        "recent": recent,
        "mostRecentChat": sessions.first(),
    }))
}

pub(crate) async fn chats(host: &Arc<Host>) -> anyhow::Result<Value> {
    let hello = host.call("hello", json!({})).await?;
    Ok(
        json!({ "sequence": decimal(hello["seq"].as_u64().unwrap_or(0)), "chats": all_chats(host).await? }),
    )
}

async fn all_chats(host: &Arc<Host>) -> anyhow::Result<Vec<Value>> {
    let agents = host.client_agents()?;
    let scopes: std::collections::HashSet<&str> = agents
        .iter()
        .map(|entry| entry.agent.scope.as_str())
        .collect();
    let sessions = host.call("sessions", json!({ "all": true })).await?;
    let mut rows = sessions
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            !row["project"]
                .as_str()
                .is_some_and(|project| scopes.contains(project))
        })
        .map(chat_item)
        .collect::<Vec<_>>();
    rows.extend(
        agents
            .iter()
            .filter(|entry| entry.agent.state != AgentState::Terminated)
            .map(agent_item),
    );
    rows.sort_by(|a, b| b["updatedAt"].as_str().cmp(&a["updatedAt"].as_str()));
    Ok(rows)
}

fn agent_item(entry: &ClientAgent) -> Value {
    let title = if entry.agent.spec.title.is_empty() {
        &entry.agent.name
    } else {
        &entry.agent.spec.title
    };
    let preview = if entry.agent.spec.description.is_empty() {
        &entry.agent.spec.role
    } else {
        &entry.agent.spec.description
    };
    json!({ "id": entry.id(), "title": title, "project": entry.project.name,
        "computer": "This computer", "updatedAt": rfc3339(entry.agent.updated_ms.max(entry.agent.created_ms)),
        "preview": preview, "participants": [entry.agent.name], "unread": false,
        "agent": { "name": entry.agent.name, "role": entry.agent.spec.role } })
}

async fn agent_conversation(host: &Arc<Host>, entry: ClientAgent) -> anyhow::Result<Value> {
    let tasks = host.client_agent_tasks(&entry)?;
    let schedules = host.client_agent_schedules(&entry)?;
    let model = host.client_agent_model(&entry).await;
    let mut messages = Vec::new();
    for task in tasks.iter().skip(tasks.len().saturating_sub(50)) {
        let at = rfc3339(task.created_ms);
        messages.push(json!({ "id": format!("{}-input", task.id), "role": "person", "author": task_sender(task),
            "body": task.objective, "createdAt": at, "delivery": "sent" }));
        if task.state.is_final() || task.state == TaskState::Suspended {
            messages.push(json!({ "id": format!("{}-answer", task.id), "role": "teammate", "author": entry.agent.name,
                "body": task_display(task), "createdAt": rfc3339(task.finished_ms.unwrap_or(task.created_ms)) }));
        }
    }
    let attention = host.call("attention_all", json!({})).await?;
    let task_ids: std::collections::HashSet<&str> =
        tasks.iter().map(|task| task.id.as_str()).collect();
    let approvals = attention["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            item["kind"] == "question"
                && item["runtime"].is_null()
                && item["project"] == entry.project.id.as_str()
                && item["agent"] == entry.agent.name
                && item["task"]
                    .as_str()
                    .is_some_and(|id| task_ids.contains(id))
        })
        .map(attention_item)
        .collect::<Vec<_>>();
    let mut chat = agent_item(&entry);
    chat["agent"]["schedules"] = json!(schedules);
    Ok(
        json!({ "chat": chat, "model": model, "messages": messages, "approvals": approvals,
        "results": host.client_results(&entry.id()).map_err(|error| anyhow::anyhow!("could not read results: {error:?}"))?,
        "activeTurnId": tasks.iter().find(|task| !task.state.is_final()).map(|task| &task.id) }),
    )
}

pub(crate) async fn conversation(host: &Arc<Host>, id: &str) -> anyhow::Result<Value> {
    if let Some(entry) = host.client_agent(id)? {
        let mut conversation = agent_conversation(host, entry).await?;
        conversation["chat"]["id"] = json!(id);
        return Ok(conversation);
    }

    let sessions = host.call("sessions", json!({ "all": true })).await?;
    let chat = sessions
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["id"] == id)
        .map(chat_item)
        .ok_or_else(|| anyhow::anyhow!("that chat no longer exists"))?;
    let read = host
        .call("chat_read", json!({ "session": id, "limit": 50 }))
        .await?;
    let model = host
        .client_conversation_model(
            id,
            read["summary"]["model"]
                .as_str()
                .unwrap_or("Current model")
                .to_string(),
            read["running"].is_null(),
        )
        .await?;
    let at = chat["updatedAt"].as_str().unwrap_or("").to_string();
    let mut messages = Vec::new();
    for turn in read["transcript"].as_array().into_iter().flatten() {
        let index = turn["index"].as_u64().unwrap_or(messages.len() as u64);
        let input = turn["input"].as_str().unwrap_or("");
        if !input.is_empty() {
            messages.push(json!({
                "id": format!("{id}-{index}-person"),
                "role": "person",
                "author": "You",
                "body": input,
                "createdAt": at,
                "delivery": "sent",
            }));
        }
        let answer = turn["answer"].as_str().unwrap_or("");
        if !answer.is_empty() {
            messages.push(json!({
                "id": format!("{id}-{index}-teammate"),
                "role": "teammate",
                "author": "ARK",
                "body": answer,
                "createdAt": at,
            }));
        }
    }
    let mut approvals = read["approvals"]
        .as_array()
        .into_iter()
        .flatten()
        .map(attention_item)
        .collect::<Vec<_>>();
    let shared = host.call("attention_all", json!({})).await?;
    for item in shared["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            item["kind"] == "question" && item["runtime"].is_null() && item["chat"] == id
        })
    {
        let projected = attention_item(item);
        if !approvals.iter().any(|prior| prior["id"] == projected["id"]) {
            approvals.push(projected);
        }
    }
    let results = host
        .client_results(id)
        .map_err(|error| anyhow::anyhow!("could not read results: {error:?}"))?;
    Ok(json!({
        "chat": chat,
        "messages": messages,
        "approvals": approvals,
        "results": results,
        "activeTurnId": read["running"].as_str().map(|_| "running"),
        "model": model,
    }))
}

pub(crate) async fn create_chat(
    host: &Arc<Host>,
    device: &str,
    project: &str,
    request_id: &str,
    model: Option<&ark_protocol::ModelSelection>,
) -> Result<Value, crate::host::ClientChatCreateError> {
    let summary = host
        .create_client_chat(device, project, request_id, model)
        .await?;
    Ok(chat_item(&summary))
}

pub(crate) async fn projects(host: &Arc<Host>) -> anyhow::Result<ark_protocol::ProjectsSnapshot> {
    let hello = host.call("hello", json!({})).await?;
    let places = host.call("places", json!({})).await?;
    let here = places["here"]
        .as_str()
        .unwrap_or("This computer")
        .to_string();
    let rows = host.call("projects", json!({})).await?;
    let projects = rows
        .as_array()
        .into_iter()
        .flatten()
        .map(|project| ark_protocol::ProjectSummary {
            id: project["id"].as_str().unwrap_or("").to_string(),
            name: project["name"].as_str().unwrap_or("Project").to_string(),
            computer: here.clone(),
            has_folder: project["workspace"]
                .as_str()
                .is_some_and(|path| !path.is_empty()),
        })
        .collect();
    Ok(ark_protocol::ProjectsSnapshot {
        sequence: ark_protocol::DecimalString::new(hello["seq"].as_u64().unwrap_or(0).to_string())
            .expect("u64 is decimal"),
        projects,
    })
}

pub(crate) async fn computers(host: &Arc<Host>) -> anyhow::Result<ark_protocol::ComputersSnapshot> {
    let places = host.call("places", json!({})).await?;
    let here = places["here"]
        .as_str()
        .unwrap_or("This computer")
        .to_string();
    let mut computers = vec![ark_protocol::ComputerSummary {
        name: here.clone(),
        state: "ok".into(),
        project_terminal: true,
        unavailable_reason: None,
    }];
    computers.extend(
        places["places"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|place| {
                let state = place["state"].as_str().unwrap_or("unavailable").to_string();
                let project_terminal = place["project_terminal"].as_bool().unwrap_or(false);
                let unavailable_reason = match (state.as_str(), project_terminal) {
                    ("ok", true) => None,
                    ("ok", false) => Some("Update ARK on this computer".into()),
                    _ => Some("This computer is not answering".into()),
                };
                ark_protocol::ComputerSummary {
                    name: place["name"].as_str().unwrap_or("Computer").to_string(),
                    state,
                    project_terminal,
                    unavailable_reason,
                }
            }),
    );
    Ok(ark_protocol::ComputersSnapshot { here, computers })
}

pub(crate) async fn terminal_open(
    host: &Arc<Host>,
    owner: &str,
    request: &ark_protocol::TerminalOpenRequest,
) -> anyhow::Result<ark_protocol::TerminalOpenResponse> {
    anyhow::ensure!(
        (2..=1000).contains(&request.cols),
        "Terminal width is out of range"
    );
    anyhow::ensure!(
        (2..=1000).contains(&request.rows),
        "Terminal height is out of range"
    );
    let value = host
        .call(
            "project_terminal_open",
            json!({
                "project": request.project_id,
                "place": request.computer.as_deref().unwrap_or(""),
                "cols": request.cols,
                "rows": request.rows,
                "big": request.copy_large_project,
                "_client_owner": owner,
            }),
        )
        .await?;
    let handle = value["ref"]["authority"]["route"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("confirm the project copy in ARK Desktop"))?
        .to_string();
    let computer = value["place"]
        .as_str()
        .unwrap_or("This computer")
        .to_string();
    Ok(ark_protocol::TerminalOpenResponse {
        handle,
        generation: required(&value, "generation")?,
        cwd: required(&value, "cwd")?,
        history: value["history"].as_str().unwrap_or("").to_string(),
        cursor: decimal_string(value["cursor"].as_u64().unwrap_or(0))?,
        ended: value["ended"].as_bool().unwrap_or(false),
        reattached: value["reattached"].as_bool().unwrap_or(false),
        project: ark_protocol::ProjectSummary {
            id: required(&value["project"], "id")?,
            name: required(&value["project"], "name")?,
            computer: computer.clone(),
            has_folder: true,
        },
        computer,
    })
}

pub(crate) async fn terminal_read(
    host: &Arc<Host>,
    owner: &str,
    request: &ark_protocol::TerminalReadRequest,
) -> anyhow::Result<ark_protocol::TerminalReadResponse> {
    let reference = host.client_terminal_ref(owner, &request.handle, &request.generation)?;
    let value = host
        .call(
            "resource",
            json!({
                "ref": reference,
                "operation": ResourceOperation {
                    kind: ResourceOperationKind::TerminalRead,
                    input: json!({ "generation": request.generation, "after": request.after.as_u64()? }),
                },
            }),
        )
        .await?;
    Ok(ark_protocol::TerminalReadResponse {
        generation: required(&value, "generation")?,
        data: value["data"].as_str().unwrap_or("").to_string(),
        base: decimal_string(value["base"].as_u64().unwrap_or(0))?,
        cursor: decimal_string(value["cursor"].as_u64().unwrap_or(0))?,
        gap: value["gap"].as_bool().unwrap_or(false),
        ended: value["ended"].as_bool().unwrap_or(false),
    })
}

pub(crate) async fn terminal_write(
    host: &Arc<Host>,
    owner: &str,
    request: &ark_protocol::TerminalWriteRequest,
) -> anyhow::Result<ark_protocol::TerminalOperationResponse> {
    anyhow::ensure!(
        request.data.len() <= ark_protocol::TERMINAL_WRITE_BYTES as usize,
        "Terminal input is too large"
    );
    terminal_operation(
        host,
        owner,
        &request.handle,
        &request.generation,
        ResourceOperationKind::TerminalWrite,
        json!({ "sequence": request.sequence.as_u64()?, "data": request.data }),
    )
    .await
}

pub(crate) async fn terminal_resize(
    host: &Arc<Host>,
    owner: &str,
    request: &ark_protocol::TerminalResizeRequest,
) -> anyhow::Result<ark_protocol::TerminalOperationResponse> {
    anyhow::ensure!(
        (2..=1000).contains(&request.cols),
        "Terminal width is out of range"
    );
    anyhow::ensure!(
        (2..=1000).contains(&request.rows),
        "Terminal height is out of range"
    );
    terminal_operation(
        host,
        owner,
        &request.handle,
        &request.generation,
        ResourceOperationKind::TerminalResize,
        json!({ "cols": request.cols, "rows": request.rows }),
    )
    .await
}

pub(crate) async fn terminal_end(
    host: &Arc<Host>,
    owner: &str,
    request: &ark_protocol::TerminalEndRequest,
) -> anyhow::Result<ark_protocol::TerminalOperationResponse> {
    terminal_operation(
        host,
        owner,
        &request.handle,
        &request.generation,
        ResourceOperationKind::TerminalClose,
        json!({}),
    )
    .await
}

async fn terminal_operation(
    host: &Arc<Host>,
    owner: &str,
    handle: &str,
    generation: &str,
    kind: ResourceOperationKind,
    input: Value,
) -> anyhow::Result<ark_protocol::TerminalOperationResponse> {
    let reference = host.client_terminal_ref(owner, handle, generation)?;
    host.call(
        "resource",
        json!({ "ref": reference, "operation": ResourceOperation { kind, input } }),
    )
    .await?;
    Ok(ark_protocol::TerminalOperationResponse { accepted: true })
}

fn decimal_string(value: u64) -> anyhow::Result<ark_protocol::DecimalString> {
    ark_protocol::DecimalString::new(value.to_string()).map_err(anyhow::Error::msg)
}

fn required(value: &Value, field: &str) -> anyhow::Result<String> {
    value[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Terminal did not return {field}"))
}

fn attention_item(item: &Value) -> Value {
    let choices = item["choices"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|choice| {
            let decision = choice["decision"].as_str().unwrap_or("deny");
            json!({
                "id": decision,
                "label": choice["label"].as_str().unwrap_or("Choose"),
                "systemImage": Value::Null,
                "destructive": decision == "deny",
            })
        })
        .collect::<Vec<_>>();
    json!({
        "id": text_id(&item["id"]),
        "kind": "approval",
        "title": item["title"].as_str().unwrap_or("ARK needs your answer"),
        "detail": item["line"].as_str().or_else(|| item["request"].as_str()).unwrap_or("Review this request."),
        "teammate": item["agent"].as_str().unwrap_or("ARK"),
        "project": item["place"].as_str().or_else(|| item["project"].as_str()).unwrap_or("ARK"),
        "computer": item["runtime"].as_str().unwrap_or("This computer"),
        "choices": choices,
    })
}

fn working_item(item: &Value) -> Value {
    let session = item["session"].as_str().unwrap_or("");
    json!({
        "id": format!("working-{session}"),
        "title": item["title"].as_str().filter(|s| !s.is_empty()).unwrap_or("Working"),
        "step": item["input"].as_str().unwrap_or("Working"),
        "teammate": "ARK",
        "computer": "This computer",
        "elapsedSeconds": 0,
        "phase": "working",
        "chatId": session,
    })
}

fn result_item(item: &Value) -> Value {
    json!({
        "id": text_id(&item["id"]),
        "title": item["title"].as_str().unwrap_or("Result"),
        "project": item["place"].as_str().unwrap_or("ARK"),
        "createdAt": rfc3339(item["at_ms"].as_u64().unwrap_or(0)),
        "kind": "text",
        "byteCount": Value::Null,
        "previewText": item["line"].as_str(),
        "downloadPath": Value::Null,
    })
}

fn chat_item(item: &Value) -> Value {
    let title = item["title"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("New chat");
    json!({
        "id": item["id"].as_str().or_else(|| item["session"]["id"].as_str()).unwrap_or(""),
        "title": title,
        "project": item["project_name"].as_str().or_else(|| item["project"]["name"].as_str()).unwrap_or("ARK"),
        "computer": "This computer",
        "updatedAt": item["updated_at"].as_str().unwrap_or("1970-01-01T00:00:00Z"),
        "preview": "",
        "participants": item["participants"].as_array(),
        "unread": false,
    })
}

fn decimal(value: u64) -> String {
    value.to_string()
}

fn text_id(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn rfc3339(milliseconds: u64) -> String {
    let nanos = i128::from(milliseconds) * 1_000_000;
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .ok()
        .and_then(|time| time.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_mapping_never_exposes_runtime_words() {
        let row = json!({
            "id": "s1", "title": "Supplier review", "project_name": "Operations",
            "updated_at": "2026-10-10T10:00:00Z"
        });
        let mapped = chat_item(&row);
        assert_eq!(mapped["title"], "Supplier review");
        assert_eq!(mapped["project"], "Operations");
        assert!(mapped.get("session").is_none());
    }
}
