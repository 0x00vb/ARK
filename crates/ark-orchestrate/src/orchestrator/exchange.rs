//! What agents said to each other, read from the store alone (P2 of the agent team plan).
//!
//! Nothing here is a store of its own. A message is one of three records that already exist:
//! an `agents.message` note (the message record, which now keeps its text), the brief of a
//! task an agent started or delegated for another, and that task's reply once it is final.

use super::failure_text;
use super::threads::own_thread;
use super::{AGENT, MESSAGE, TASK};
use crate::graph::MessageEdge;
use crate::{AgentRecord, OrchError, Origin, TaskRecord, TaskState};
use ark_store::{scrub_text, Store};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A message record as it is stored: the graph's edge plus the words.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct MessageRecord {
    #[serde(flatten)]
    pub edge: MessageEdge,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

/// One thing one agent said to another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Said {
    /// The agent that started the exchange: whose task sent the note or the brief.
    pub initiator: String,
    /// The agent that was asked.
    pub other: String,
    /// Who wrote this: the initiator for a note or a brief, the other for a reply.
    pub author: String,
    pub text: String,
    pub at_ms: u64,
    /// The initiator's task that made it. A chat's turn is such a task.
    pub turn: String,
    /// The conversation of the initiator's task: its own thread, or `None` for its default one.
    pub lane: Option<String>,
    /// The task that carried the work (the brief and its reply); `None` for a note.
    pub work: Option<String>,
    /// A reply that never came: the task ended in an error.
    pub failed: bool,
    /// A brief whose task has not ended: its reply may still come.
    pub pending: bool,
}

/// An agent as an exchange names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    /// The title the person gave it, else empty.
    pub title: String,
    pub look: String,
    /// Its default conversation.
    pub session: Option<String>,
}

/// The agents of `project` by name.
pub fn known(store: &Store, project: &str) -> Result<HashMap<String, Known>, OrchError> {
    let mut out = HashMap::new();
    for value in store.list_records(AGENT, Some(project))? {
        match serde_json::from_value::<AgentRecord>(value) {
            Ok(a) if a.project == project => {
                out.insert(
                    a.name,
                    Known {
                        title: a.spec.title,
                        look: a.spec.look,
                        session: a.session,
                    },
                );
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(project, "skipping a damaged agent record: {error}"),
        }
    }
    Ok(out)
}

/// Every message between two agents of `project`, oldest first. The person's own messages,
/// work with no agent behind it and notes saved before their words were kept are not here.
pub fn said(store: &Store, project: &str) -> Result<Vec<Said>, OrchError> {
    let sessions = known(store, project)?;
    let mut tasks: HashMap<String, TaskRecord> = HashMap::new();
    for value in store.list_records(TASK, Some(project))? {
        match serde_json::from_value::<TaskRecord>(value) {
            Ok(t) if t.project == project => {
                tasks.insert(t.id.clone(), t);
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(project, "skipping a damaged task record: {error}"),
        }
    }
    let lane = |task: &str| -> Option<String> {
        let t = tasks.get(task)?;
        let session = sessions.get(&t.owner)?.session.as_deref();
        own_thread(t, session).map(str::to_string)
    };
    let mut asked: Vec<&TaskRecord> = tasks.values().collect();
    asked.sort_by(|a, b| (a.created_ms, &a.id).cmp(&(b.created_ms, &b.id)));
    let mut out = Vec::new();
    for t in asked {
        let Origin::Agent { agent, task } = &t.origin else {
            continue;
        };
        if agent == &t.owner || !sessions.contains_key(agent) || !sessions.contains_key(&t.owner) {
            continue;
        }
        let lane = lane(task);
        let said = |author: &str, text: String, at_ms: u64, failed: bool, pending: bool| Said {
            initiator: agent.clone(),
            other: t.owner.clone(),
            author: author.to_string(),
            text,
            at_ms,
            turn: task.clone(),
            lane: lane.clone(),
            work: Some(t.id.clone()),
            failed,
            pending,
        };
        out.push(said(
            agent,
            scrub_text(&t.objective),
            t.created_ms,
            false,
            !t.state.is_final(),
        ));
        let at = t.finished_ms.unwrap_or(t.created_ms);
        match t.state {
            TaskState::Done => {
                if let Some(text) = t.result.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                    out.push(said(&t.owner, scrub_text(text), at, false, false));
                }
            }
            TaskState::Failed => {
                let text = match t.result.as_deref() {
                    Some(r) if !r.trim().is_empty() => scrub_text(&failure_text(t)),
                    _ => String::new(),
                };
                out.push(said(&t.owner, text, at, true, false));
            }
            _ => {}
        }
    }
    for value in store.list_records(MESSAGE, Some(project))? {
        let m = match serde_json::from_value::<MessageRecord>(value) {
            Ok(m) => m,
            Err(error) => {
                tracing::warn!(project, "skipping a damaged message record: {error}");
                continue;
            }
        };
        let Some(task) = m.edge.task.clone() else {
            continue;
        };
        if m.text.is_empty()
            || m.edge.from == m.edge.to
            || !sessions.contains_key(&m.edge.from)
            || !sessions.contains_key(&m.edge.to)
        {
            continue;
        }
        out.push(Said {
            lane: lane(&task),
            initiator: m.edge.from.clone(),
            other: m.edge.to,
            author: m.edge.from,
            text: scrub_text(&m.text),
            at_ms: m.edge.at_ms,
            turn: task,
            work: None,
            failed: false,
            pending: false,
        });
    }
    // A brief comes before its reply even in one millisecond; the sort is stable.
    out.sort_by_key(|s| s.at_ms);
    Ok(out)
}
