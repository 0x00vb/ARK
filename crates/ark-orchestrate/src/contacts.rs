use crate::{AgentRecord, AgentState, OrchError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentContact {
    pub name: String,
    pub role: String,
    pub state: AgentState,
    pub created_ms: u64,
    pub updated_ms: u64,
}

/// Read identities without opening conversations, resolving models or starting work.
pub fn agent_contacts(
    store: &ark_store::Store,
    project: &str,
) -> Result<Vec<AgentContact>, OrchError> {
    let mut contacts = Vec::new();
    for value in store.list_records(crate::orchestrator::AGENT, Some(project))? {
        let record: AgentRecord = serde_json::from_value(value)?;
        if record.project != project {
            return Err(OrchError::NotAllowed(
                "agent belongs to another project".into(),
            ));
        }
        contacts.push(AgentContact {
            name: record.name,
            role: record.spec.role,
            state: record.state,
            created_ms: record.created_ms,
            updated_ms: record.updated_ms,
        });
    }
    contacts.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(contacts)
}
