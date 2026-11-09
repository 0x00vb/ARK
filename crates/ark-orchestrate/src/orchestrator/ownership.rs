use super::*;

pub(super) struct ExecutionLease {
    engine: Arc<Engine>,
    project: String,
    task: String,
    token: String,
}

impl Drop for ExecutionLease {
    fn drop(&mut self) {
        let result = self
            .engine
            .store
            .transact_records::<_, OrchError>(&self.project, |tx| {
                let Some(value) = tx.get_record(TASK, &self.task)? else {
                    return Ok(());
                };
                let mut record: TaskRecord = serde_json::from_value(value)?;
                if record.execution_owner.as_deref() != Some(&self.token) {
                    return Ok(());
                }
                record.execution_owner = None;
                record.pid = None;
                record.pid_start = None;
                tx.put_record(TASK, &self.task, &serde_json::to_value(&record)?)?;
                Ok(())
            });
        if let Err(error) = result {
            tracing::error!(task = %self.task, %error, "could not release work execution ownership");
        }
    }
}

impl Inner {
    /// One task at a time per conversation of an agent (`lane`, see [`Inner::task_lane`]),
    /// across every orchestrator of this store in the process.
    pub(super) fn agent_execution_lock(
        &self,
        agent: &str,
        lane: &str,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, OrchError> {
        type Key = (std::path::PathBuf, String, String, String);
        type Locks = Mutex<HashMap<Key, Weak<tokio::sync::Mutex<()>>>>;
        static LOCKS: std::sync::OnceLock<Locks> = std::sync::OnceLock::new();
        let root =
            std::fs::canonicalize(self.engine.store.root()).map_err(ark_store::StoreError::from)?;
        let mut locks = LOCKS.get_or_init(Default::default).lock().unwrap();
        let key = (
            root,
            self.project.id.to_string(),
            agent.to_string(),
            lane.to_string(),
        );
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        Ok(lock)
    }

    pub(super) fn reserve_execution(
        &self,
        id: &str,
    ) -> Result<(TaskRecord, Option<ExecutionLease>), OrchError> {
        let (record, acquired) =
            self.engine
                .store
                .transact_records(self.project.id.as_str(), |tx| {
                    let value = tx
                        .get_record(TASK, id)?
                        .ok_or_else(|| OrchError::UnknownTask(id.into()))?;
                    let mut record: TaskRecord = serde_json::from_value(value)?;
                    let alive = record.execution_owner.is_some()
                        && record
                            .pid
                            .is_some_and(|pid| process::writer_alive(pid, record.pid_start));
                    let repair = record.state != TaskState::Done
                        && tx.get_record(results::RESULT, id)?.is_some();
                    if (record.state.is_final() && !repair) || alive {
                        return Ok::<_, OrchError>((record, false));
                    }
                    if repair && record.state.is_final() {
                        record.state = TaskState::Queued;
                    }
                    record.execution_owner = Some(self.execution_token.clone());
                    record.pid = Some(std::process::id());
                    record.pid_start = process::own_start();
                    tx.put_record(TASK, id, &serde_json::to_value(&record)?)?;
                    Ok((record, true))
                })?;
        let lease = acquired.then(|| ExecutionLease {
            engine: self.engine.clone(),
            project: self.project.id.to_string(),
            task: id.to_string(),
            token: self.execution_token.clone(),
        });
        Ok((record, lease))
    }

    pub(super) fn require_agent_control(&self, agent: &str) -> Result<(), OrchError> {
        self.engine
            .store
            .transact_records::<_, OrchError>(self.project.id.as_str(), |tx| {
                for value in tx.list_records(TASK)? {
                    let task: TaskRecord = serde_json::from_value(value)?;
                    if task.owner == agent
                        && task.submission.is_some()
                        && task
                            .execution_owner
                            .as_ref()
                            .is_some_and(|owner| owner != &self.execution_token)
                        && task
                            .pid
                            .is_some_and(|pid| process::writer_alive(pid, task.pid_start))
                    {
                        return Err(OrchError::NotAllowed(
                            "this agent has work in another live runtime; use its connection"
                                .into(),
                        ));
                    }
                }
                Ok(())
            })
    }

    pub(super) fn require_execution_control(&self, id: &str) -> Result<(), OrchError> {
        let value = self
            .engine
            .store
            .get_record(TASK, id)?
            .ok_or_else(|| OrchError::UnknownTask(id.into()))?;
        let record: TaskRecord = serde_json::from_value(value)?;
        if record.project != self.project.id.as_str() {
            return Err(OrchError::NotAllowed(
                "the task belongs to another project".into(),
            ));
        }
        if record.submission.is_some()
            && record
                .execution_owner
                .as_ref()
                .is_some_and(|owner| owner != &self.execution_token)
            && record
                .pid
                .is_some_and(|pid| process::writer_alive(pid, record.pid_start))
        {
            return Err(OrchError::NotAllowed(
                "this work is running in another live runtime; use its connection".into(),
            ));
        }
        Ok(())
    }
}

pub(super) fn schedule_lock(engine: &Engine, project: &str) -> Result<Arc<Mutex<()>>, OrchError> {
    type Key = (std::path::PathBuf, String);
    type Locks = Mutex<HashMap<Key, Weak<Mutex<()>>>>;
    static LOCKS: std::sync::OnceLock<Locks> = std::sync::OnceLock::new();
    let root = std::fs::canonicalize(engine.store.root()).map_err(ark_store::StoreError::from)?;
    let mut locks = LOCKS.get_or_init(Default::default).lock().unwrap();
    let key = (root, project.into());
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}
