use super::*;
use ark_store::RecordTransaction;

const SETTLED: &str = "orch_work_settled";

pub(super) struct WorkData {
    pub boards: HashMap<String, Board>,
    pub items: HashMap<String, WorkItem>,
    pub tasks: HashMap<String, TaskRecord>,
    agents: HashMap<String, AgentRecord>,
}

impl WorkData {
    pub fn load(tx: &RecordTransaction<'_>) -> Result<Self, OrchError> {
        let boards = tx
            .list_records(BOARD)?
            .into_iter()
            .map(|value| serde_json::from_value::<Board>(value).map(|b| (b.id.clone(), b)))
            .collect::<Result<_, _>>()?;
        let items = tx
            .list_records(WORK)?
            .into_iter()
            .map(|value| serde_json::from_value::<WorkItem>(value).map(|w| (w.id.clone(), w)))
            .collect::<Result<_, _>>()?;
        let tasks = tx
            .list_records(TASK)?
            .into_iter()
            .map(|value| serde_json::from_value::<TaskRecord>(value).map(|t| (t.id.clone(), t)))
            .collect::<Result<_, _>>()?;
        let agents = tx
            .list_records(AGENT)?
            .into_iter()
            .map(|value| serde_json::from_value::<AgentRecord>(value).map(|a| (a.name.clone(), a)))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            boards,
            items,
            tasks,
            agents,
        })
    }

    pub fn repair_boards(
        &mut self,
        tx: &RecordTransaction<'_>,
        project: &str,
    ) -> Result<(), OrchError> {
        if self
            .items
            .values()
            .all(|item| self.boards.contains_key(&item.board))
        {
            return Ok(());
        }
        let first = self.ensure_board(tx, project, PERSON, FIRST_GOAL, None)?;
        for item in self
            .items
            .values_mut()
            .filter(|item| !self.boards.contains_key(&item.board))
        {
            item.board = first.id.clone();
            tx.put_record(WORK, &item.id, &serde_json::to_value(&item)?)?;
        }
        Ok(())
    }

    pub(super) fn ensure_board(
        &mut self,
        tx: &RecordTransaction<'_>,
        project: &str,
        by: &str,
        goal: &str,
        chat: Option<String>,
    ) -> Result<Board, OrchError> {
        if let Some(board) = self.boards.values().find(|board| board.is_goal(goal)) {
            return Ok(board.clone());
        }
        let board = Board::new(project, by, goal.trim(), chat);
        tx.insert_record(BOARD, &board.id, &serde_json::to_value(&board)?)?;
        self.boards.insert(board.id.clone(), board.clone());
        Ok(board)
    }

    fn rank(&self, item: &WorkItem, before: Option<&str>) -> f64 {
        let mut column: Vec<_> = self
            .items
            .values()
            .filter(|other| {
                other.board == item.board && other.state == item.state && other.id != item.id
            })
            .collect();
        column.sort_by(|a, b| {
            a.rank
                .partial_cmp(&b.rank)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        rank_before(&column, before)
    }

    fn apply(&self, item: &mut WorkItem, by: &str, change: WorkChange) -> Result<(), OrchError> {
        if let Some(title) = change.title {
            if title.trim().is_empty() {
                return Err(OrchError::NotAllowed("an item needs a title".into()));
            }
            item.title = clip(title.trim(), 200);
        }
        if let Some(note) = change.note {
            item.note = clip(&note, 4000);
        }
        if let Some(owner) = change.owner {
            let owner = match owner.trim() {
                "" => None,
                PERSON | "you" | "person" => Some(PERSON.to_string()),
                name => {
                    if !self
                        .agents
                        .get(name)
                        .is_some_and(|a| a.state != AgentState::Terminated)
                    {
                        return Err(OrchError::NotAllowed(format!("no agent named `{name}`")));
                    }
                    Some(name.to_string())
                }
            };
            if owner != item.owner {
                if let Some(task) = item
                    .tasks
                    .iter()
                    .rev()
                    .filter_map(|id| self.tasks.get(id))
                    .find(|task| !task.state.is_final())
                {
                    return Err(OrchError::NotAllowed(format!(
                        "{} is working on it ({}); cancel that task first",
                        task.owner, task.id
                    )));
                }
            }
            item.set_owner(by, owner);
        }
        if let Some(blocked) = change.blocked {
            item.set_blocked(by, Some(blocked));
        }
        Ok(())
    }
}

pub(super) fn claimable(
    tasks: &HashMap<String, TaskRecord>,
    item: &WorkItem,
    owner: &str,
) -> Result<(), OrchError> {
    if let Some(task) = blocking_task(tasks, item, owner) {
        return Err(OrchError::NotAllowed(format!(
            "{} is being worked on by {} ({}); wait for it or pick another item",
            item.id, task.owner, task.id
        )));
    }
    Ok(())
}

pub(super) fn blocking_task<'a>(
    tasks: &'a HashMap<String, TaskRecord>,
    item: &WorkItem,
    owner: &str,
) -> Option<&'a TaskRecord> {
    item.tasks
        .iter()
        .rev()
        .filter_map(|id| tasks.get(id))
        .find(|task| !task.state.is_final() && task.owner != owner)
}

fn changed(item: &WorkItem, by: &str) -> EventKind {
    EventKind::Work {
        item: item.clone(),
        by: by.to_string(),
    }
}

impl Inner {
    pub(super) fn with_work<T>(
        &self,
        operation: impl FnOnce(
            &mut WorkData,
            &RecordTransaction<'_>,
        ) -> Result<(T, Vec<EventKind>), OrchError>,
    ) -> Result<T, OrchError> {
        let mut state = self.state.lock().unwrap();
        let (result, events, data) =
            self.engine
                .store
                .transact_records(self.project.id.as_str(), |tx| {
                    let mut data = WorkData::load(tx)?;
                    let (result, events) = operation(&mut data, tx)?;
                    Ok::<_, OrchError>((result, events, data))
                })?;
        state.work = data.items;
        state.boards = data.boards;
        for event in events {
            self.emit(event);
        }
        Ok(result)
    }

    pub(super) fn retry_record(&self, id: &str) -> Result<(), OrchError> {
        let mut state = self.state.lock().unwrap();
        if !state.tasks.contains_key(id) {
            return Err(OrchError::UnknownTask(id.into()));
        }
        let (record, data, event) =
            self.engine
                .store
                .transact_records(self.project.id.as_str(), |tx| {
                    let mut data = WorkData::load(tx)?;
                    let mut task = data
                        .tasks
                        .get(id)
                        .cloned()
                        .ok_or_else(|| OrchError::UnknownTask(id.into()))?;
            if task.submission.is_some() && task.result.is_some() {
                return Err(OrchError::NotAllowed("this task already has an answer; restore its saved result instead of repeating the work".into()));
            }
                    if task.run.is_some() { return Err(OrchError::NotAllowed("retry this step through its automation execution".into())); }
                    if task.execution_owner.is_some()
                        && task
                            .pid
                            .is_some_and(|pid| process::writer_alive(pid, task.pid_start))
                    {
                        return Err(OrchError::NotAllowed(
                            "the task is still finishing; try again when it has stopped".into(),
                        ));
                    }
                    if !matches!(task.state, TaskState::Failed | TaskState::Cancelled) {
                        return Err(OrchError::NotAllowed(format!(
                            "task {id} is {}; only a failed or cancelled task runs again",
                            task.state.as_str()
                        )));
                    }
                    let event = if let Some(item) =
                        task.item.as_ref().and_then(|id| data.items.get_mut(id))
                    {
                        claimable(&data.tasks, item, &task.owner)?;
                        item.blocked = None;
                        if item.state != WorkState::Doing {
                            item.state = WorkState::Doing;
                            item.done_ms = None;
                        }
                        item.step(&task.owner, format!("running {} again", task.id));
                        tx.put_record(WORK, &item.id, &serde_json::to_value(&item)?)?;
                        tx.delete_record(SETTLED, &format!("{}/{}", item.id, task.id))?;
                        Some(changed(item, &task.owner))
                    } else {
                        None
                    };
                    task.state = TaskState::Queued;
                    task.error = None;
                    task.finished_ms = None;
                    task.started_ms = None;
                    task.reported = false;
                    task.max_attempts = task.attempts.saturating_add(1);
                    tx.put_record(TASK, id, &serde_json::to_value(&task)?)?;
                    Ok::<_, OrchError>((task, data, event))
                })?;
        state.work = data.items;
        state.boards = data.boards;
        let task = state.tasks.get_mut(id).expect("checked task exists");
        task.record = record;
        task.stop = None;
        task.cancel = CancelToken::new();
        task.done.send_replace(false);
        if let Some(event) = event {
            self.emit(event);
        }
        self.emit(EventKind::TaskQueued {
            task: id.to_string(),
            reason: "retry".into(),
        });
        Ok(())
    }

    pub(super) fn work_follows(&self, task: &TaskRecord) -> Result<(), OrchError> {
        let Some(id) = &task.item else {
            return Ok(());
        };
        self.with_work(|data, tx| {
            let Some(task) = data.tasks.get(&task.id) else {
                return Ok(((), vec![]));
            };
            if !task.state.is_final() {
                return Ok(((), vec![]));
            }
            let Some(item) = data.items.get_mut(id) else {
                return Ok(((), vec![]));
            };
            if item.run.is_some() {
                return Ok(((), vec![]));
            }
            let receipt_id = format!("{id}/{}", task.id);
            let receipt = serde_json::json!([
                task.state,
                task.result,
                task.error,
                task.finished_ms,
                task.outputs
            ]);
            if tx.get_record(SETTLED, &receipt_id)?.as_ref() == Some(&receipt) {
                return Ok(((), vec![]));
            }
            let others_open = item
                .tasks
                .iter()
                .filter(|id| *id != &task.id)
                .filter_map(|id| data.tasks.get(id))
                .any(|task| !task.state.is_final());
            match task.state {
                TaskState::Done => {
                    let said = clip(task.result.as_deref().unwrap_or(""), 200);
                    item.step(&task.owner, format!("finished {}: {said}", task.id));
                    if !others_open && item.state != WorkState::Done {
                        item.state = WorkState::Done;
                        item.done_ms = Some(now_ms());
                        item.blocked = None;
                    }
                }
                TaskState::Failed => {
                    let why = format!(
                        "{} failed: {}",
                        task.id,
                        clip(task.error.as_deref().unwrap_or("no reason given"), 200)
                    );
                    item.step(&task.owner, why.clone());
                    if item.state != WorkState::Done {
                        item.blocked = Some(why);
                    }
                }
                TaskState::Cancelled => {
                    item.step(&task.owner, format!("{} was cancelled", task.id));
                    if !others_open && item.state == WorkState::Doing {
                        item.state = WorkState::Todo;
                    }
                }
                _ => return Ok(((), vec![])),
            }
            tx.put_record(WORK, id, &serde_json::to_value(&item)?)?;
            tx.put_record(SETTLED, &receipt_id, &receipt)?;
            Ok(((), vec![changed(item, &task.owner)]))
        })
    }

    pub(super) fn archive_work(
        &self,
        user: &str,
        id: &str,
        archived: bool,
    ) -> Result<WorkItem, OrchError> {
        self.with_work(|data, tx| {
            let item = data
                .items
                .get_mut(id)
                .ok_or_else(|| OrchError::NotAllowed(format!("no board item `{id}`")))?;
            item.archived_by.retain(|who| who != user);
            if archived {
                item.archived_by.push(user.to_string());
            }
            tx.put_record(WORK, id, &serde_json::to_value(&item)?)?;
            Ok((item.clone(), vec![changed(item, user)]))
        })
    }

    pub(crate) fn add_work(
        &self,
        by: &str,
        title: &str,
        mut change: WorkChange,
    ) -> Result<WorkItem, OrchError> {
        if title.trim().is_empty() {
            return Err(OrchError::NotAllowed("an item needs a title".into()));
        }
        self.with_work(|data, tx| {
            let mut events = vec![];
            let board = match change.board.take().filter(|id| !id.trim().is_empty()) {
                Some(id) => data
                    .boards
                    .get(id.trim())
                    .cloned()
                    .ok_or_else(|| OrchError::NotAllowed(format!("no board `{id}`")))?,
                None => match board::latest_first(
                    data.boards.values().cloned().collect(),
                    &data.items.values().cloned().collect::<Vec<_>>(),
                )
                .into_iter()
                .next()
                {
                    Some(board) => board,
                    None => {
                        let board =
                            data.ensure_board(tx, self.project.id.as_str(), by, FIRST_GOAL, None)?;
                        events.push(EventKind::Board {
                            board: board.clone(),
                            by: by.to_string(),
                        });
                        board
                    }
                },
            };
            let mut item = WorkItem::new(self.project.id.as_str(), &board.id, by, title.trim());
            change.title = None;
            item.state = change.state.take().unwrap_or(WorkState::Todo);
            if item.state == WorkState::Done {
                item.done_ms = Some(now_ms());
            }
            item.rank = data.rank(&item, change.before.take().as_deref());
            data.apply(&mut item, by, change)?;
            tx.insert_record(WORK, &item.id, &serde_json::to_value(&item)?)?;
            data.items.insert(item.id.clone(), item.clone());
            events.push(changed(&item, by));
            Ok((item, events))
        })
    }

    pub(crate) fn change_work(
        &self,
        by: &str,
        id: &str,
        mut change: WorkChange,
    ) -> Result<WorkItem, OrchError> {
        self.with_work(|data, tx| {
            let mut item = data
                .items
                .get(id)
                .cloned()
                .ok_or_else(|| OrchError::NotAllowed(format!("no board item `{id}`")))?;
            if by == PERSON && change.state.is_some_and(|state| state != item.state) {
                let active_task = item.tasks.iter().any(|id| {
                    data.tasks
                        .get(id)
                        .is_some_and(|task| !task.state.is_final())
                });
                let active_run = if let Some(run) = &item.run {
                    let value = tx.get_record(crate::automation::RUN, run)?.ok_or_else(|| {
                        OrchError::NotAllowed("the automation execution is missing".into())
                    })?;
                    !serde_json::from_value::<crate::automation::Run>(value)?
                        .phase
                        .is_final()
                } else {
                    false
                };
                let mut active_general = false;
                for value in tx.list_records(admission::SUBMISSION)? {
                    let submission: admission::Submission = serde_json::from_value(value)?;
                    if submission.receipt.work == item.id
                        && matches!(submission.target, WorkTarget::General { .. })
                    {
                        active_general |= matches!(
                            general::state(tx, &submission.receipt)?,
                            GeneralWorkState::Pending
                                | GeneralWorkState::Starting
                                | GeneralWorkState::Running
                        );
                    }
                }
                if active_task || active_run || active_general {
                    return Err(OrchError::NotAllowed(
                        "wait for this execution to finish before changing its status".into(),
                    ));
                }
            }
            let to = change
                .board
                .take()
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty() && *id != item.board);
            if let Some(to) = &to {
                let board = data
                    .boards
                    .get(to)
                    .ok_or_else(|| OrchError::NotAllowed(format!("no board `{to}`")))?;
                item.board = to.clone();
                item.step(by, format!("moved it to the board \"{}\"", board.goal));
            }
            let moved = change.state.is_some() || change.before.is_some() || to.is_some();
            if let Some(state) = change.state.take() {
                item.set_state(by, state);
            }
            if moved {
                item.rank = data.rank(&item, change.before.as_deref());
            }
            change.before = None;
            data.apply(&mut item, by, change)?;
            item.updated_ms = now_ms();
            tx.put_record(WORK, id, &serde_json::to_value(&item)?)?;
            data.items.insert(id.to_string(), item.clone());
            Ok((item.clone(), vec![changed(&item, by)]))
        })
    }

    pub(crate) fn remove_work(&self, id: &str) -> Result<WorkItem, OrchError> {
        self.with_work(|data, tx| {
            let item = data
                .items
                .remove(id)
                .ok_or_else(|| OrchError::NotAllowed(format!("no board item `{id}`")))?;
            tx.delete_record(WORK, id)?;
            let event = EventKind::WorkRemoved {
                item: item.id.clone(),
                title: item.title.clone(),
            };
            Ok((item, vec![event]))
        })
    }

    pub(crate) fn add_board(
        &self,
        by: &str,
        goal: &str,
        chat: Option<String>,
    ) -> Result<Board, OrchError> {
        if goal.trim().is_empty() {
            return Err(OrchError::NotAllowed("a board needs a goal".into()));
        }
        self.with_work(|data, tx| {
            if let Some(board) = data.boards.values().find(|board| board.is_goal(goal)) {
                return Ok((board.clone(), vec![]));
            }
            let board = data.ensure_board(tx, self.project.id.as_str(), by, goal, chat)?;
            Ok((
                board.clone(),
                vec![EventKind::Board {
                    board,
                    by: by.to_string(),
                }],
            ))
        })
    }

    pub(crate) fn change_board(&self, by: &str, id: &str, goal: &str) -> Result<Board, OrchError> {
        if goal.trim().is_empty() {
            return Err(OrchError::NotAllowed("a board needs a goal".into()));
        }
        self.with_work(|data, tx| {
            if data
                .boards
                .values()
                .any(|board| board.id != id && board.is_goal(goal))
            {
                return Err(OrchError::NotAllowed(format!(
                    "another board has the goal \"{goal}\""
                )));
            }
            let board = data
                .boards
                .get_mut(id)
                .ok_or_else(|| OrchError::NotAllowed(format!("no board `{id}`")))?;
            board.goal = clip(goal.trim(), 120);
            tx.put_record(BOARD, id, &serde_json::to_value(&board)?)?;
            Ok((
                board.clone(),
                vec![EventKind::Board {
                    board: board.clone(),
                    by: by.to_string(),
                }],
            ))
        })
    }

    pub(crate) fn remove_board(&self, id: &str) -> Result<Board, OrchError> {
        self.with_work(|data, tx| {
            let board = data
                .boards
                .remove(id)
                .ok_or_else(|| OrchError::NotAllowed(format!("no board `{id}`")))?;
            let ids: Vec<_> = data
                .items
                .values()
                .filter(|item| item.board == id)
                .map(|item| item.id.clone())
                .collect();
            let mut events = vec![];
            for id in ids {
                let item = data.items.remove(&id).expect("listed item exists");
                tx.delete_record(WORK, &id)?;
                events.push(EventKind::WorkRemoved {
                    item: id,
                    title: item.title,
                });
            }
            tx.delete_record(BOARD, id)?;
            events.push(EventKind::BoardRemoved {
                board: board.id.clone(),
                goal: board.goal.clone(),
            });
            Ok((board, events))
        })
    }
}
