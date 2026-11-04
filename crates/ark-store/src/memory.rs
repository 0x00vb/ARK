use ark_core::{MemorySource, MemoryState, NoteId, ProjectId, ProjectMemory, SessionId};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use time::OffsetDateTime;

use crate::{Result, Store, StoreError};

const KIND: &str = "project_memory";
const MAX_RECORDS: usize = 512;

pub enum MemoryReview {
    Accept,
    Reject,
    Forget,
}

fn invalid(message: &str) -> StoreError {
    StoreError::Memory(message.into())
}

fn read(db: &Connection, project: &ProjectId, id: &str) -> Result<ProjectMemory> {
    let raw: Option<String> = db
        .query_row(
            "SELECT json FROM records WHERE kind=?1 AND scope=?2 AND id=?3",
            params![KIND, project.as_str(), id],
            |row| row.get(0),
        )
        .optional()?;
    let memory: ProjectMemory =
        serde_json::from_str(&raw.ok_or_else(|| invalid("memory not found in this project"))?)?;
    if memory.project != *project || memory.id.as_str() != id {
        return Err(invalid("memory scope is invalid"));
    }
    Ok(memory)
}

fn write(db: &Connection, memory: &ProjectMemory) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO records(kind,id,scope,updated_at,json) VALUES(?1,?2,?3,?4,?5)",
        params![
            KIND,
            memory.id.as_str(),
            memory.project.as_str(),
            memory.updated_at.to_string(),
            serde_json::to_string(memory)?
        ],
    )?;
    Ok(())
}

fn scoped(db: &Connection, project: &ProjectId) -> Result<Vec<ProjectMemory>> {
    let mut stmt =
        db.prepare("SELECT json FROM records WHERE kind=?1 AND scope=?2 AND COALESCE(json_extract(json, '$.state.kind'),'') NOT IN ('rejected','superseded','forgotten') ORDER BY rowid")?;
    let rows = stmt.query_map(params![KIND, project.as_str()], |row| {
        row.get::<_, String>(0)
    })?;
    let mut out = Vec::new();
    for row in rows {
        if out.len() >= MAX_RECORDS {
            return Err(invalid("project memory limit exceeded"));
        }
        let memory: ProjectMemory = serde_json::from_str(&row?)?;
        if memory.project != *project {
            return Err(invalid("memory scope is invalid"));
        }
        out.push(memory);
    }
    Ok(out)
}

fn activate(db: &Connection, memory: &mut ProjectMemory, reviewer: &SessionId) -> Result<()> {
    if scoped(db, &memory.project)?.iter().any(|m| {
        m.id != memory.id
            && m.topic.eq_ignore_ascii_case(&memory.topic)
            && m.state == MemoryState::Active
            && memory.replaces.as_ref() != Some(&m.id)
    }) {
        return Err(invalid(
            "this topic changed; propose an explicit replacement",
        ));
    }
    if let Some(id) = &memory.replaces {
        let mut previous = read(db, &memory.project, id.as_str())?;
        if previous.state != MemoryState::Active {
            return Err(invalid(
                "the previous memory changed; review its current replacement",
            ));
        }
        previous.state = MemoryState::Superseded {
            by: memory.id.clone(),
        };
        previous.updated_at = memory.updated_at;
        write(db, &previous)?;
    }
    memory.state = MemoryState::Active;
    memory.reviewed_by = Some(reviewer.clone());
    Ok(())
}

impl Store {
    /// Active and pending memory only. Retired records remain addressable by exact id.
    pub fn project_memories(&self, project: &ProjectId) -> Result<Vec<ProjectMemory>> {
        scoped(&self.db.lock().unwrap(), project)
    }

    pub fn project_memory(&self, project: &ProjectId, id: &str) -> Result<ProjectMemory> {
        read(&self.db.lock().unwrap(), project, id)
    }

    /// A person's correction is active; an agent's suggestion always waits for review.
    /// Replacement and its predecessor's retirement are one transaction.
    pub fn record_memory(
        &self,
        project: &ProjectId,
        topic: &str,
        body: &str,
        source: MemorySource,
        replaces: Option<NoteId>,
    ) -> Result<ProjectMemory> {
        let topic = topic.trim();
        let body = body.trim();
        if topic.is_empty() || topic.len() > 128 || topic.contains(['\n', '\r', '|']) {
            return Err(invalid(
                "topic must be one line of 1 to 128 bytes without |",
            ));
        }
        if body.is_empty() || body.len() > 2000 {
            return Err(invalid("correction must contain 1 to 2000 bytes"));
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project.as_str()],
            |row| row.get(0),
        )?;
        if !project_exists {
            return Err(invalid("project not found"));
        }
        match &source {
            MemorySource::Person { session } => {
                let valid: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND project_id=?2)",
                    params![session.as_str(), project.as_str()],
                    |row| row.get(0),
                )?;
                if !valid {
                    return Err(invalid("source chat not found in this project"));
                }
            }
            MemorySource::Agent { invocation } => {
                let valid: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM invocations WHERE id=?1 AND project_id=?2)",
                    params![invocation.as_str(), project.as_str()],
                    |row| row.get(0),
                )?;
                if !valid {
                    return Err(invalid("source call not found in this project"));
                }
            }
        }
        let records = scoped(&tx, project)?;
        for existing in &records {
            if existing.topic.eq_ignore_ascii_case(topic)
                && existing.body == body
                && existing.replaces == replaces
            {
                if let MemorySource::Person { session } = &source {
                    let mut existing = existing.clone();
                    if existing.state == MemoryState::Pending {
                        existing.updated_at = OffsetDateTime::now_utc();
                        activate(&tx, &mut existing, session)?;
                        write(&tx, &existing)?;
                        tx.commit()?;
                    }
                    return Ok(existing);
                }
                if existing.state == MemoryState::Pending {
                    return Ok(existing.clone());
                }
            }
        }
        if records.len() >= MAX_RECORDS
            && !(replaces.is_some() && matches!(source, MemorySource::Person { .. }))
        {
            return Err(invalid(
                "project has 512 active or pending memories; review or forget one first",
            ));
        }
        if let Some(id) = &replaces {
            let previous = read(&tx, project, id.as_str())?;
            if previous.state != MemoryState::Active || !previous.topic.eq_ignore_ascii_case(topic)
            {
                return Err(invalid(
                    "replacement must name an active memory with the same topic",
                ));
            }
        } else if records
            .iter()
            .any(|m| m.topic.eq_ignore_ascii_case(topic) && m.state == MemoryState::Active)
        {
            return Err(invalid(
                "this topic already has a memory; replace it explicitly",
            ));
        }
        let now = OffsetDateTime::now_utc();
        let mut memory = ProjectMemory {
            id: NoteId::new(),
            project: project.clone(),
            topic: topic.into(),
            body: body.into(),
            source,
            state: MemoryState::Pending,
            replaces,
            reviewed_by: None,
            created_at: now,
            updated_at: now,
        };
        if let MemorySource::Person { session } = &memory.source {
            let reviewer = session.clone();
            activate(&tx, &mut memory, &reviewer)?;
        }
        write(&tx, &memory)?;
        tx.commit()?;
        Ok(memory)
    }

    pub fn review_memory(
        &self,
        project: &ProjectId,
        id: &str,
        review: MemoryReview,
        reviewer: &SessionId,
    ) -> Result<ProjectMemory> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut memory = read(&tx, project, id)?;
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND project_id=?2)",
            params![reviewer.as_str(), project.as_str()],
            |row| row.get(0),
        )?;
        if !valid {
            return Err(invalid("review chat not found in this project"));
        }
        memory.updated_at = OffsetDateTime::now_utc();
        match review {
            MemoryReview::Forget => {
                memory.topic.clear();
                memory.body.clear();
                memory.state = MemoryState::Forgotten;
                memory.reviewed_by = Some(reviewer.clone());
            }
            MemoryReview::Accept if memory.state == MemoryState::Active => return Ok(memory),
            MemoryReview::Reject if memory.state == MemoryState::Rejected => return Ok(memory),
            MemoryReview::Accept | MemoryReview::Reject if memory.state != MemoryState::Pending => {
                return Err(invalid("only a pending memory can be reviewed"))
            }
            MemoryReview::Accept => {
                if scoped(&tx, project)?.iter().any(|m| {
                    m.topic.eq_ignore_ascii_case(&memory.topic)
                        && m.state == MemoryState::Active
                        && memory.replaces.as_ref() != Some(&m.id)
                }) {
                    return Err(invalid(
                        "this topic changed; propose an explicit replacement",
                    ));
                }
                activate(&tx, &mut memory, reviewer)?;
            }
            MemoryReview::Reject => {
                memory.state = MemoryState::Rejected;
                memory.reviewed_by = Some(reviewer.clone());
            }
        }
        write(&tx, &memory)?;
        tx.commit()?;
        Ok(memory)
    }
}
