use rusqlite::{OptionalExtension, Transaction, TransactionBehavior};
use serde_json::Value;

use crate::{rfc, Result, Store, StoreError};

/// A scoped unit of work. Keep its callback synchronous and free of external effects.
/// Calling Store again from the callback would acquire the same database mutex.
pub struct RecordTransaction<'a> {
    tx: Transaction<'a>,
    scope: &'a str,
}

impl Store {
    /// Read, validate and write scoped records in one immediate transaction.
    /// Publish in-memory state and start execution only after this returns Ok.
    pub fn transact_records<T, E>(
        &self,
        scope: &str,
        operation: impl FnOnce(&RecordTransaction<'_>) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<StoreError>,
    {
        let mut db = self.db.lock().unwrap();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        let transaction = RecordTransaction { tx, scope };
        let result = operation(&transaction)?;
        transaction.tx.commit().map_err(StoreError::from)?;
        Ok(result)
    }
}

impl RecordTransaction<'_> {
    fn execution_project(&self, project: &str) -> Result<()> {
        if project == self.scope {
            return Ok(());
        }
        let parent: Option<Option<String>> = self
            .tx
            .query_row(
                "SELECT json_extract(json,'$.parent') FROM projects WHERE id=?1",
                [project],
                |row| row.get(0),
            )
            .optional()?;
        if parent.flatten().as_deref() == Some(self.scope) {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "execution belongs to another project",
        )
        .into())
    }

    pub fn validate_execution_project(&self, id: &str) -> Result<()> {
        self.execution_project(id)
    }

    pub fn create_execution_project(&self, project: &ark_core::Project) -> Result<()> {
        if project.parent.as_ref().map(|parent| parent.as_str()) != Some(self.scope) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "an execution project needs this parent",
            )
            .into());
        }
        self.tx.execute(
            "INSERT INTO projects(id,created_at,json) VALUES(?1,?2,?3)",
            rusqlite::params![
                project.id.as_str(),
                rfc(project.created_at),
                serde_json::to_string(project)?
            ],
        )?;
        Ok(())
    }

    pub fn event_with_fields(&self, stream: &str, fields: &[(&str, Value)]) -> Result<Option<u64>> {
        if fields.is_empty()
            || fields.len() > 16
            || fields.iter().any(|(field, _)| {
                !field
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "event matching needs one to sixteen scalar fields",
            )
            .into());
        }
        let mut sql = "SELECT seq FROM events WHERE stream=?".to_string();
        let mut values = vec![rusqlite::types::Value::Text(stream.into())];
        for (field, value) in fields {
            if value.is_array() || value.is_object() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "event matching requires scalar values",
                )
                .into());
            }
            sql.push_str(" AND json_extract(json, ?) IS json_extract(?, '$')");
            values.push(rusqlite::types::Value::Text(format!("$.{field}")));
            values.push(rusqlite::types::Value::Text(serde_json::to_string(value)?));
        }
        sql.push_str(" ORDER BY seq LIMIT 1");
        Ok(self
            .tx
            .query_row(&sql, rusqlite::params_from_iter(values.iter()), |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn append_event(&self, stream: &str, value: &Value) -> Result<u64> {
        self.tx.execute(
            "INSERT INTO events(stream, at, json) VALUES(?1, ?2, ?3)",
            rusqlite::params![
                stream,
                rfc(time::OffsetDateTime::now_utc()),
                serde_json::to_string(value)?
            ],
        )?;
        Ok(self.tx.last_insert_rowid() as u64)
    }

    pub fn create_execution_session(&self, id: &str, record: &Value) -> Result<()> {
        let project = record["project"].as_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "an execution conversation needs a project",
            )
        })?;
        self.execution_project(project)?;
        self.tx.execute(
            "INSERT INTO sessions(id,project_id,updated_at,json) VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                id,
                project,
                rfc(time::OffsetDateTime::now_utc()),
                serde_json::to_string(record)?
            ],
        )?;
        Ok(())
    }

    pub fn get_execution_session(&self, id: &str) -> Result<Option<Value>> {
        let row: Option<(String, String)> = self
            .tx
            .query_row(
                "SELECT project_id,json FROM sessions WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            Some((project, json)) => {
                self.execution_project(&project)?;
                Ok(Some(serde_json::from_str(&json)?))
            }
            None => Ok(None),
        }
    }

    pub fn get_execution_turn(&self, id: &str, index: u32) -> Result<Option<Value>> {
        if self.get_execution_session(id)?.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the execution conversation was deleted",
            )
            .into());
        }
        let row: Option<String> = self
            .tx
            .query_row(
                "SELECT json FROM session_turns WHERE session_id=?1 AND idx=?2",
                rusqlite::params![id, index],
                |row| row.get(0),
            )
            .optional()?;
        row.map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn execution_invocations(
        &self,
        project: &str,
        work: &str,
    ) -> Result<Vec<ark_core::Invocation>> {
        self.execution_project(project)?;
        let mut statement = self.tx.prepare("SELECT json FROM invocations WHERE project_id=?1 AND json_extract(json,'$.work')=?2 ORDER BY created_at,id")?;
        let rows = statement.query_map([project, work], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn execution_invocation(
        &self,
        project: &str,
        id: &str,
    ) -> Result<Option<ark_core::Invocation>> {
        self.execution_project(project)?;
        let row: Option<String> = self
            .tx
            .query_row(
                "SELECT json FROM invocations WHERE project_id=?1 AND id=?2",
                [project, id],
                |row| row.get(0),
            )
            .optional()?;
        row.map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn execution_artifact(
        &self,
        project: &str,
        id: &str,
    ) -> Result<Option<ark_core::Artifact>> {
        self.execution_project(project)?;
        let row: Option<String> = self
            .tx
            .query_row(
                "SELECT json FROM artifacts WHERE project_id=?1 AND id=?2",
                [project, id],
                |row| row.get(0),
            )
            .optional()?;
        row.map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn quota_used(&self, key: &str, window_start: u64) -> Result<Option<u32>> {
        Ok(self
            .tx
            .query_row(
                "SELECT used FROM quota_windows WHERE quota_key=?1 AND window_start=?2",
                rusqlite::params![key, window_start],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn reserve_quota(&self, key: &str, window_start: u64, limit: u32) -> Result<bool> {
        if key.is_empty() || key.len() > 256 || limit == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a quota needs a bounded key and a positive limit",
            )
            .into());
        }
        Ok(self.tx.execute(
            "INSERT INTO quota_windows(quota_key,window_start,used) VALUES(?1,?2,1)
             ON CONFLICT(quota_key,window_start) DO UPDATE SET used=used+1 WHERE used<?3",
            rusqlite::params![key, window_start, limit],
        )? == 1)
    }

    pub fn put_artifact(&self, artifact: &ark_core::Artifact) -> Result<()> {
        self.tx.execute("INSERT OR REPLACE INTO artifacts(project_id,id,kind,name,created_at,json) VALUES(?1,?2,?3,?4,?5,?6)", rusqlite::params![self.scope,artifact.id.as_str(),artifact.kind.as_tag(),artifact.name,rfc(artifact.created_at),serde_json::to_string(artifact)?])?;
        Ok(())
    }

    pub fn get_artifact(&self, id: &str) -> Result<Option<ark_core::Artifact>> {
        let value: Option<String> = self
            .tx
            .query_row(
                "SELECT json FROM artifacts WHERE project_id=?1 AND id=?2",
                [self.scope, id],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn invocations_of_work(&self, work: &str) -> Result<Vec<ark_core::Invocation>> {
        let mut statement = self.tx.prepare("SELECT json FROM invocations WHERE project_id=?1 AND json_extract(json,'$.work')=?2 ORDER BY created_at ASC")?;
        let rows = statement.query_map([self.scope, work], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    pub fn get_session(&self, id: &str) -> Result<Option<Value>> {
        let row: Option<(String, String)> = self
            .tx
            .query_row(
                "SELECT project_id,json FROM sessions WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some((project, json)) if project == self.scope => {
                Ok(Some(serde_json::from_str(&json)?))
            }
            Some(_) => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "conversation belongs to another project",
            )
            .into()),
        }
    }
    pub fn get_record(&self, kind: &str, id: &str) -> Result<Option<Value>> {
        let row: Option<(String, String)> = self
            .tx
            .query_row(
                "SELECT scope,json FROM records WHERE kind=?1 AND id=?2",
                [kind, id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some((scope, json)) if scope == self.scope => Ok(Some(serde_json::from_str(&json)?)),
            Some(_) => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "record belongs to another project",
            )
            .into()),
        }
    }

    pub fn records_matching(&self, kind: &str, field: &str, value: &str) -> Result<Vec<Value>> {
        let mut statement = self.tx.prepare(
            "SELECT json FROM records WHERE kind=?1 AND scope=?2 AND json_extract(json,?3)=?4 ORDER BY id",
        )?;
        let rows = statement.query_map(
            rusqlite::params![kind, self.scope, format!("$.{field}"), value],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn records_matching_states(
        &self,
        kind: &str,
        field: &str,
        value: &str,
        state_field: &str,
        states: &[&str],
        limit: usize,
    ) -> Result<Vec<Value>> {
        let mut statement = self.tx.prepare("SELECT json FROM records WHERE kind=?1 AND scope=?2 AND json_extract(json,?3)=?4 AND json_extract(json,?5) IN (SELECT value FROM json_each(?6)) ORDER BY id LIMIT ?7")?;
        let rows = statement.query_map(
            rusqlite::params![
                kind,
                self.scope,
                format!("$.{field}"),
                value,
                format!("$.{state_field}"),
                serde_json::to_string(states)?,
                limit.clamp(1, 1001) as i64,
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn records_states_after(
        &self,
        kind: &str,
        field: &str,
        states: &[&str],
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let mut statement = self.tx.prepare("SELECT json FROM records WHERE kind=?1 AND scope=?2 AND json_extract(json,?3) IN (SELECT value FROM json_each(?4)) AND id>?5 ORDER BY id LIMIT ?6")?;
        let rows = statement.query_map(
            rusqlite::params![
                kind,
                self.scope,
                format!("$.{field}"),
                serde_json::to_string(states)?,
                after.unwrap_or(""),
                limit.clamp(1, 100) as i64
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn records_matching_due(
        &self,
        kind: &str,
        field: &str,
        value: &str,
        due: &str,
        now: u64,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let mut statement=self.tx.prepare("SELECT json FROM records WHERE kind=?1 AND scope=?2 AND json_extract(json,?3)=?4 AND COALESCE(json_extract(json,?5),0)<=?6 ORDER BY COALESCE(json_extract(json,?5),0),id LIMIT ?7")?;
        let rows = statement.query_map(
            rusqlite::params![
                kind,
                self.scope,
                format!("$.{field}"),
                value,
                format!("$.{due}"),
                now,
                limit.clamp(1, 1001) as i64
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn records_overlapping(
        &self,
        kind: &str,
        start: &str,
        end: &str,
        window: std::ops::Range<u64>,
        required: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        if window.start >= window.end {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the record window is empty",
            )
            .into());
        }
        let mut statement = self.tx.prepare("SELECT json FROM records WHERE kind=?1 AND scope=?2 AND json_extract(json,?3)<?5 AND (json_extract(json,?4) IS NULL OR json_extract(json,?4)>?6) AND (?7 IS NULL OR json_extract(json,?7) IS NOT NULL) ORDER BY json_extract(json,?3),id LIMIT ?8")?;
        let rows = statement.query_map(
            rusqlite::params![
                kind,
                self.scope,
                format!("$.{start}"),
                format!("$.{end}"),
                window.end,
                window.start,
                required.map(|field| format!("$.{field}")),
                limit.clamp(1, 1001) as i64
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn records_page(
        &self,
        kind: &str,
        field: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        self.records_page_filtered(kind, field, after, limit, None)
    }

    pub fn records_page_matching(
        &self,
        kind: &str,
        filter_field: &str,
        filter_value: &str,
        order: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        self.records_page_filtered(
            kind,
            order,
            after,
            limit,
            Some((filter_field, filter_value)),
        )
    }

    fn records_page_filtered(
        &self,
        kind: &str,
        field: &str,
        after: Option<&str>,
        limit: usize,
        filter: Option<(&str, &str)>,
    ) -> Result<Vec<Value>> {
        let path = format!("$.{field}");
        let cursor = after
            .map(serde_json::from_str::<(u64, String)>)
            .transpose()?;
        if let Some((stamp, id)) = &cursor {
            let current = self.get_record(kind, id)?;
            if !current.as_ref().is_some_and(|record| {
                record.get(field).and_then(Value::as_u64) == Some(*stamp)
                    && filter.is_none_or(|(field, value)| {
                        record.get(field).and_then(Value::as_str) == Some(value)
                    })
            }) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the list changed; refresh it",
                )
                .into());
            }
        }
        let value = cursor.as_ref().map_or(0, |(value, _)| *value);
        let after_id = cursor.as_ref().map(|(_, id)| id.as_str());
        let mut statement = self.tx.prepare(
            "SELECT json FROM records WHERE kind=?1 AND scope=?2 AND (?7 IS NULL OR json_extract(json,?7)=?8) AND (?4 IS NULL OR COALESCE(json_extract(json,?3),0)<?5 OR (COALESCE(json_extract(json,?3),0)=?5 AND id>?4)) ORDER BY COALESCE(json_extract(json,?3),0) DESC,id LIMIT ?6",
        )?;
        let rows = statement.query_map(
            rusqlite::params![
                kind,
                self.scope,
                path,
                after_id,
                value,
                limit.clamp(1, 1001) as i64,
                filter.map(|(field, _)| format!("$.{field}")),
                filter.map(|(_, value)| value),
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn list_records(&self, kind: &str) -> Result<Vec<Value>> {
        let mut statement = self.tx.prepare(
            "SELECT json FROM records WHERE kind=?1 AND scope=?2 ORDER BY updated_at,id",
        )?;
        let rows = statement.query_map([kind, self.scope], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn put_record(&self, kind: &str, id: &str, value: &Value) -> Result<()> {
        self.get_record(kind, id)?;
        self.tx.execute(
            "INSERT INTO records(kind,id,scope,updated_at,json) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(kind,id) DO UPDATE SET updated_at=excluded.updated_at,json=excluded.json",
            rusqlite::params![
                kind,
                id,
                self.scope,
                rfc(time::OffsetDateTime::now_utc()),
                serde_json::to_string(value)?
            ],
        )?;
        Ok(())
    }

    pub fn insert_record(&self, kind: &str, id: &str, value: &Value) -> Result<()> {
        self.tx.execute(
            "INSERT INTO records(kind,id,scope,updated_at,json) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![
                kind,
                id,
                self.scope,
                rfc(time::OffsetDateTime::now_utc()),
                serde_json::to_string(value)?
            ],
        )?;
        Ok(())
    }

    pub fn delete_record(&self, kind: &str, id: &str) -> Result<bool> {
        self.get_record(kind, id)?;
        Ok(self.tx.execute(
            "DELETE FROM records WHERE kind=?1 AND id=?2 AND scope=?3",
            [kind, id, self.scope],
        )? > 0)
    }

    pub fn create_session(&self, id: &str, record: &Value) -> Result<()> {
        self.tx.execute(
            "INSERT INTO sessions(id,project_id,updated_at,json) VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                id,
                self.scope,
                rfc(time::OffsetDateTime::now_utc()),
                serde_json::to_string(record)?
            ],
        )?;
        Ok(())
    }

    pub fn put_session(&self, id: &str, record: &Value) -> Result<()> {
        if self.get_session(id)?.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "conversation was deleted",
            )
            .into());
        }
        self.tx.execute(
            "UPDATE sessions SET updated_at=?1,json=?2 WHERE id=?3 AND project_id=?4",
            rusqlite::params![
                rfc(time::OffsetDateTime::now_utc()),
                serde_json::to_string(record)?,
                id,
                self.scope
            ],
        )?;
        Ok(())
    }

    pub fn get_session_turn(&self, id: &str, index: u32) -> Result<Option<Value>> {
        if self.get_session(id)?.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "conversation was deleted",
            )
            .into());
        }
        let value: Option<String> = self
            .tx
            .query_row(
                "SELECT json FROM session_turns WHERE session_id=?1 AND idx=?2",
                rusqlite::params![id, index],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn put_session_turn(&self, id: &str, index: u32, turn: &Value) -> Result<()> {
        if self.get_session(id)?.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "conversation was deleted",
            )
            .into());
        }
        self.tx.execute(
            "INSERT INTO session_turns(session_id,idx,json,text_len) VALUES(?1,?2,?3,?4)
             ON CONFLICT(session_id,idx) DO UPDATE SET json=excluded.json,text_len=excluded.text_len",
            rusqlite::params![id, index, serde_json::to_string(turn)?, (turn["input"].as_str().map_or(0,str::len) + turn["answer"].as_str().map_or(0,str::len)) as i64],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Barrier};

    #[test]
    fn quota_is_shared_across_project_scopes_and_rolls_back_with_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert!(store
            .transact_records::<_, StoreError>("a", |tx| tx.reserve_quota("account", 1000, 2))
            .unwrap());
        let failed = store.transact_records::<_, StoreError>("b", |tx| {
            assert!(tx.reserve_quota("account", 1000, 2)?);
            tx.insert_record("receipt", "x", &json!({}))?;
            Err::<(), _>(std::io::Error::other("injected admission failure").into())
        });
        assert!(failed.is_err());
        assert!(store.get_record("receipt", "x").unwrap().is_none());
        assert!(store
            .transact_records::<_, StoreError>("b", |tx| tx.reserve_quota("account", 1000, 2))
            .unwrap());
        assert!(!store
            .transact_records::<_, StoreError>("c", |tx| tx.reserve_quota("account", 1000, 2))
            .unwrap());
        store
            .db
            .lock()
            .unwrap()
            .execute_batch("DROP TABLE quota_windows")
            .unwrap();
        assert!(store
            .transact_records::<_, StoreError>("a", |tx| tx.reserve_quota("account", 1000, 2))
            .is_err());
    }

    #[test]
    fn page_cursor_preserves_its_stamp_and_refuses_changes_or_foreign_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .put_record("work", "a", "p", &json!({"updated_ms":100}))
            .unwrap();
        store
            .put_record("work", "b", "p", &json!({"updated_ms":90}))
            .unwrap();
        let cursor = serde_json::to_string(&(100_u64, "a")).unwrap();
        store
            .transact_records::<_, StoreError>("p", |tx| {
                assert_eq!(
                    tx.records_page("work", "updated_ms", None, 1)?,
                    vec![json!({"updated_ms":100})]
                );
                assert_eq!(
                    tx.records_page("work", "updated_ms", Some(&cursor), 1)?,
                    vec![json!({"updated_ms":90})]
                );
                Ok(())
            })
            .unwrap();
        for stamp in [110, 70] {
            store
                .put_record("work", "a", "p", &json!({"updated_ms":stamp}))
                .unwrap();
            assert!(store
                .transact_records::<_, StoreError>("p", |tx| tx.records_page(
                    "work",
                    "updated_ms",
                    Some(&cursor),
                    1
                ))
                .is_err());
        }
        assert!(store
            .transact_records::<_, StoreError>("q", |tx| tx.records_page(
                "work",
                "updated_ms",
                Some(&cursor),
                1
            ))
            .is_err());
    }

    #[test]
    fn concurrent_connections_admit_one_receipt_with_its_task() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path()).unwrap();
        let second = Store::open(dir.path()).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let run = |store: Store, task: &'static str| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .transact_records::<_, StoreError>("p", |tx| {
                        if let Some(receipt) = tx.get_record("receipt", "request")? {
                            return Ok(receipt);
                        }
                        tx.insert_record("task", task, &json!(task))?;
                        tx.insert_record("receipt", "request", &json!(task))?;
                        Ok(json!(task))
                    })
                    .unwrap()
            })
        };
        let a = run(first, "a");
        let b = run(second, "b");
        assert_eq!(a.join().unwrap(), b.join().unwrap());
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.list_records("task", Some("p")).unwrap().len(), 1);
    }

    #[test]
    fn errors_rollback_records_and_session_and_foreign_scope_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.put_record("item", "foreign", "q", &json!(0)).unwrap();
        let result = store.transact_records::<_, StoreError>("p", |tx| {
            tx.insert_record("task", "t", &json!(1))?;
            tx.create_session("s", &json!({"id":"s"}))?;
            tx.put_record("item", "foreign", &json!(1))
        });
        assert!(result.is_err());
        assert!(store.get_record("task", "t").unwrap().is_none());
        assert!(store.get_session("s").unwrap().is_none());
        assert_eq!(store.get_record("item", "foreign").unwrap(), Some(json!(0)));
        store.db.lock().unwrap().execute_batch(
            "CREATE TRIGGER reject_receipt BEFORE INSERT ON records WHEN NEW.kind='receipt' BEGIN SELECT RAISE(ABORT,'injected'); END;"
        ).unwrap();
        assert!(store
            .transact_records::<_, StoreError>("p", |tx| {
                tx.insert_record("task", "t", &json!(1))?;
                tx.create_session("s", &json!({"id":"s"}))?;
                tx.insert_record("receipt", "request", &json!(1))
            })
            .is_err());
        assert!(store.get_record("task", "t").unwrap().is_none());
        assert!(store.get_session("s").unwrap().is_none());
    }
}
