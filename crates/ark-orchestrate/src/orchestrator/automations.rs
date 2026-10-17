//! Event automations on the orchestrator (ADR 0062): create, pause, resume, change, discard and
//! stop, the evaluator that turns saved events into runs, and what happens when a run ends. The
//! rules are in [`crate::automation`].

use super::*;
use crate::automation::{
    event_brief, run_task_id, AutomationState, EventAutomationRecord, EventEvaluation,
    EventPattern, Outcome, StoredEvent, AUTOMATION, CLOCK_SKEW_MS, EVALUATION,
};

/// Tries after a run fails for a reason that may pass, as a schedule's runs get.
const RETRIES: u32 = crate::schedule::DEFAULT_RETRIES;

/// Events read from the log per step of an evaluation.
const READ: usize = 256;

/// What [`Orchestrator::change_automation`] changes; `None` keeps it.
#[derive(Clone, Debug, Default)]
pub struct AutomationChange {
    pub objective: Option<String>,
    pub context: Option<String>,
    pub pattern: Option<EventPattern>,
}

/// What a cutover ends: matched events still waiting get a final outcome, and a run made but
/// not begun is cancelled once that is saved.
#[derive(Default)]
struct Cutting {
    evals: Vec<EventEvaluation>,
    tasks: Vec<String>,
}

/// How a cutover ends the matched events still waiting.
#[derive(Clone, Copy)]
enum Cut {
    Pause,
    Change,
    Stop,
    Discard,
}

impl Cut {
    fn outcome(self) -> Outcome {
        match self {
            Cut::Pause => Outcome::CancelledByPause,
            Cut::Change => Outcome::CancelledByChange,
            Cut::Stop => Outcome::CancelledByStop,
            Cut::Discard => Outcome::Discarded,
        }
    }
}

impl Orchestrator {
    /// Hand `agent` this task each time an event `pattern` matches arrives, from now on (see
    /// [`crate::automation`]). The agent must be ARK's own (not an agent program) and do nothing
    /// else: event content stays out of every other task.
    pub fn automate(
        &self,
        agent: &str,
        task: TaskSpec,
        pattern: EventPattern,
    ) -> Result<EventAutomationRecord, OrchError> {
        if task.objective.trim().is_empty() {
            return Err(OrchError::NotAllowed("a task needs an objective".into()));
        }
        if pattern.source.trim().is_empty() || pattern.account.trim().is_empty() {
            return Err(OrchError::NotAllowed(
                "an automation needs the source and the account its events come from".into(),
            ));
        }
        let _one = self.inner.evals.lock().unwrap();
        {
            let st = self.inner.state.lock().unwrap();
            let a = st
                .agents
                .get(agent)
                .ok_or_else(|| OrchError::UnknownAgent(agent.into()))?;
            if a.record.state == AgentState::Terminated {
                return Err(OrchError::Terminated(agent.into()));
            }
            if a.record
                .spec
                .model
                .as_deref()
                .is_some_and(|m| m.starts_with("agent:"))
            {
                return Err(OrchError::NotAllowed(format!(
                    "`{agent}` runs on another agent program; an automation runs on ARK's own agent"
                )));
            }
            let standing = st.automations.values().any(|x| x.agent == agent)
                || st.schedules.values().any(|s| s.agent == agent);
            let open = st
                .tasks
                .values()
                .any(|t| t.record.owner == agent && !t.record.state.is_final());
            if standing || open {
                return Err(OrchError::NotAllowed(format!(
                    "`{agent}` already has other work; an automation needs an agent of its own"
                )));
            }
        }
        let now = now_ms();
        let seq = self.inner.log.current_seq();
        let rec = EventAutomationRecord {
            id: format!("a-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
            project: self.inner.project.id.to_string(),
            agent: agent.to_string(),
            task,
            pattern,
            state: AutomationState::Listening,
            revision: 1,
            activated_seq: seq,
            activated_ms: now,
            evaluated_seq: seq,
            created_ms: now,
            matched: 0,
            runs: 0,
            last_event: None,
            last_event_ms: None,
            last_task: None,
            failures: 0,
            counted: None,
        };
        self.inner.persist(Some(&rec), &[])?;
        self.inner.keep_automation(&rec);
        self.inner.changed_automation(&rec);
        Ok(rec)
    }

    /// The automations kept, oldest first.
    pub fn automations(&self) -> Vec<EventAutomationRecord> {
        let st = self.inner.state.lock().unwrap();
        let mut v: Vec<EventAutomationRecord> = st.automations.values().cloned().collect();
        v.sort_by_key(|a| (a.created_ms, a.id.clone()));
        v
    }

    pub fn automation(&self, id: &str) -> Option<EventAutomationRecord> {
        self.inner
            .state
            .lock()
            .unwrap()
            .automations
            .get(id)
            .cloned()
    }

    /// Matched events waiting for the open run to end, oldest first.
    pub fn automation_pending(&self, id: &str) -> Vec<EventEvaluation> {
        self.inner.pending_of(id)
    }

    /// The automation's runs, newest first.
    pub fn automation_runs(&self, id: &str, limit: usize) -> Vec<TaskRecord> {
        let st = self.inner.state.lock().unwrap();
        let mut v: Vec<TaskRecord> = st
            .tasks
            .values()
            .filter(|t| t.record.automation.as_deref() == Some(id))
            .map(|t| t.record.clone())
            .collect();
        v.sort_by_key(|t| std::cmp::Reverse((t.created_ms, t.id.clone())));
        v.truncate(limit);
        v
    }

    /// Every matched event of an automation and what became of it, oldest first.
    pub fn automation_evaluations(&self, id: &str) -> Vec<EventEvaluation> {
        let st = self.inner.state.lock().unwrap();
        let mut v: Vec<EventEvaluation> = st
            .evaluations
            .values()
            .filter(|e| e.automation == id)
            .cloned()
            .collect();
        v.sort_by_key(|e| (e.seq, e.revision));
        v
    }

    /// Let events go until resumed. Matched events still waiting and a run not started yet are
    /// cancelled; a run already working goes on.
    pub fn pause_automation(&self, id: &str) -> Result<EventAutomationRecord, OrchError> {
        self.inner.alter(id, |inner, a| {
            if a.state.paused() {
                return Ok(Cutting::default());
            }
            a.state = AutomationState::Paused;
            Ok(inner.cut(a, Cut::Pause))
        })
    }

    /// Listen again, from now: events that came while it was paused do not run.
    pub fn resume_automation(&self, id: &str) -> Result<EventAutomationRecord, OrchError> {
        let rec = self.inner.alter(id, |inner, a| {
            if !a.state.paused() {
                return Ok(Cutting::default());
            }
            a.state = AutomationState::Listening;
            a.failures = 0;
            inner.cut_over(a);
            Ok(Cutting::default())
        })?;
        self.inner.evaluate();
        Ok(rec)
    }

    /// Change what runs or which events start it. Matched events still waiting are cancelled
    /// and only events from now on match the new words; a run working goes on as it was handed.
    pub fn change_automation(
        &self,
        id: &str,
        change: AutomationChange,
    ) -> Result<EventAutomationRecord, OrchError> {
        let rec = self.inner.alter(id, |inner, a| {
            if let Some(o) = change.objective.as_deref().map(str::trim) {
                if o.is_empty() {
                    return Err(OrchError::NotAllowed("a task needs an objective".into()));
                }
                a.task.objective = o.to_string();
            }
            if let Some(c) = &change.context {
                a.task.context = c.trim().to_string();
            }
            if let Some(p) = &change.pattern {
                if p.source.trim().is_empty() || p.account.trim().is_empty() {
                    return Err(OrchError::NotAllowed(
                        "an automation needs the source and the account its events come from"
                            .into(),
                    ));
                }
                a.pattern = p.clone();
            }
            let cancelled = inner.cut(a, Cut::Change);
            if !a.state.paused() {
                a.state = AutomationState::Listening;
            }
            inner.cut_over(a);
            Ok(cancelled)
        })?;
        self.inner.evaluate();
        Ok(rec)
    }

    /// Drop the matched events waiting (a Full queue included) and listen from now.
    pub fn discard_pending(&self, id: &str) -> Result<EventAutomationRecord, OrchError> {
        let rec = self.inner.alter(id, |inner, a| {
            let cancelled = inner.cut(a, Cut::Discard);
            if !a.state.paused() {
                a.state = AutomationState::Listening;
            }
            inner.cut_over(a);
            Ok(cancelled)
        })?;
        self.inner.evaluate();
        Ok(rec)
    }

    /// Stop an automation. Matched events waiting and a run not started yet are cancelled; a
    /// run working goes on; its runs and their results stay.
    pub fn unautomate(&self, id: &str) -> Result<EventAutomationRecord, OrchError> {
        let _one = self.inner.evals.lock().unwrap();
        let found = self
            .inner
            .state
            .lock()
            .unwrap()
            .automations
            .get(id)
            .cloned();
        let a = found.ok_or_else(|| self.inner.no_automation(id))?;
        let cancelled = self.inner.cut(&a, Cut::Stop);
        self.inner.persist(None, &cancelled.evals)?;
        self.inner
            .engine
            .store
            .delete_record(AUTOMATION, &a.id)
            .map_err(ark_runtime::AgentError::from)?;
        {
            let mut st = self.inner.state.lock().unwrap();
            st.automations.remove(&a.id);
            for e in &cancelled.evals {
                st.evaluations.insert(e.id.clone(), e.clone());
            }
        }
        self.inner.log.forget(&a.id);
        self.inner.cancel_unstarted(&cancelled.tasks);
        self.inner.emit(EventKind::Unautomated {
            automation: a.id.clone(),
            agent: a.agent.clone(),
        });
        Ok(a)
    }

    /// [`unautomate`](Self::unautomate), and end its agent when it has no open task.
    pub async fn stop_automation(&self, id: &str) -> Result<EventAutomationRecord, OrchError> {
        let rec = self.unautomate(id)?;
        let busy = self
            .agent(&rec.agent)
            .is_some_and(|a| a.current_task.is_some() || a.state == AgentState::Running);
        if !busy {
            self.terminate(&rec.agent).await?;
        }
        Ok(rec)
    }

    /// Match the saved events no automation of this project has looked at yet and start the
    /// runs that are due; returns the tasks started. The evaluator calls this when events
    /// arrive and when a run ends; a test can too.
    pub fn evaluate_events(&self) -> Vec<String> {
        self.inner.evaluate()
    }

    /// Run [`evaluate_events`](Self::evaluate_events) now and each time the event log grows,
    /// while the orchestrator lives. [`OrchestratorBuilder::build`] starts one inside a tokio
    /// runtime.
    pub fn spawn_event_evaluator(&self) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(&self.inner);
        let mut grown = self.inner.log.subscribe();
        tokio::spawn(async move {
            loop {
                {
                    let Some(inner) = weak.upgrade() else { break };
                    inner.evaluate();
                }
                if grown.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}

impl Inner {
    fn no_automation(&self, id: &str) -> OrchError {
        let known: Vec<String> = self
            .state
            .lock()
            .unwrap()
            .automations
            .keys()
            .cloned()
            .collect();
        OrchError::NotAllowed(if known.is_empty() {
            format!("no automation `{id}`; there are none")
        } else {
            format!(
                "no automation `{id}`; the automations are {}",
                known.join(", ")
            )
        })
    }

    /// Save an automation and evaluations in one transaction.
    fn persist(
        &self,
        a: Option<&EventAutomationRecord>,
        evals: &[EventEvaluation],
    ) -> Result<(), OrchError> {
        let project = self.project.id.as_str();
        let mut values = Vec::new();
        if let Some(a) = a {
            values.push((AUTOMATION, a.id.clone(), serde_json::to_value(a)?));
        }
        for e in evals {
            values.push((EVALUATION, e.id.clone(), serde_json::to_value(e)?));
        }
        let rows: Vec<(&str, &str, &str, &serde_json::Value)> = values
            .iter()
            .map(|(k, id, v)| (*k, id.as_str(), project, v))
            .collect();
        self.engine
            .store
            .put_records(&rows)
            .map_err(ark_runtime::AgentError::from)?;
        Ok(())
    }

    fn keep_automation(&self, a: &EventAutomationRecord) {
        self.state
            .lock()
            .unwrap()
            .automations
            .insert(a.id.clone(), a.clone());
        self.log.mark(a);
    }

    fn changed_automation(&self, a: &EventAutomationRecord) {
        self.emit(EventKind::AutomationChanged {
            automation: a.id.clone(),
            agent: a.agent.clone(),
            says: a.pattern.describe(),
            state: a.state,
        });
    }

    /// Change an automation under the evaluator's lock and save it with the evaluations `f`
    /// cancelled. Nothing changes when `f` refuses.
    fn alter(
        &self,
        id: &str,
        f: impl FnOnce(&Inner, &mut EventAutomationRecord) -> Result<Cutting, OrchError>,
    ) -> Result<EventAutomationRecord, OrchError> {
        let _one = self.evals.lock().unwrap();
        let found = self.state.lock().unwrap().automations.get(id).cloned();
        let mut a = found.ok_or_else(|| self.no_automation(id))?;
        let cancelled = f(self, &mut a)?;
        self.persist(Some(&a), &cancelled.evals)?;
        {
            let mut st = self.state.lock().unwrap();
            for e in &cancelled.evals {
                st.evaluations.insert(e.id.clone(), e.clone());
            }
        }
        self.keep_automation(&a);
        self.cancel_unstarted(&cancelled.tasks);
        self.changed_automation(&a);
        Ok(a)
    }

    /// A new revision from now: only events after this point match it.
    fn cut_over(&self, a: &mut EventAutomationRecord) {
        let seq = self.log.current_seq();
        a.revision += 1;
        a.activated_seq = seq;
        a.evaluated_seq = seq;
        a.activated_ms = now_ms();
    }

    /// What of `a` has not started: its waiting matched events, and its run when that has not
    /// begun. Nothing changes until the caller saves the evaluations and cancels the tasks; a
    /// run working goes on.
    fn cut(&self, a: &EventAutomationRecord, how: Cut) -> Cutting {
        let (out, unstarted) = {
            let st = self.state.lock().unwrap();
            let unstarted: Vec<String> = st
                .tasks
                .values()
                .filter(|t| {
                    t.record.automation.as_deref() == Some(a.id.as_str())
                        && !t.record.state.is_final()
                        && t.record.attempts == 0
                        && !t.recovering
                })
                .map(|t| t.record.id.clone())
                .collect();
            let out: Vec<EventEvaluation> = st
                .evaluations
                .values()
                .filter(|e| e.automation == a.id)
                .filter(|e| match &e.outcome {
                    Outcome::Pending => true,
                    Outcome::Started { task } => unstarted.contains(task),
                    _ => false,
                })
                .map(|e| EventEvaluation {
                    outcome: how.outcome(),
                    ..e.clone()
                })
                .collect();
            (out, unstarted)
        };
        Cutting {
            evals: out,
            tasks: unstarted,
        }
    }

    fn cancel_unstarted(&self, tasks: &[String]) {
        for t in tasks {
            self.cancel_tree(t, Stop::Cancel);
        }
    }

    fn pending_of(&self, id: &str) -> Vec<EventEvaluation> {
        let st = self.state.lock().unwrap();
        let mut v: Vec<EventEvaluation> = st
            .evaluations
            .values()
            .filter(|e| e.automation == id && e.outcome == Outcome::Pending)
            .cloned()
            .collect();
        v.sort_by_key(|e| e.seq);
        v
    }

    /// See [`Orchestrator::evaluate_events`]. One evaluation or change at a time.
    pub(crate) fn evaluate(&self) -> Vec<String> {
        let _one = self.evals.lock().unwrap();
        let list: Vec<EventAutomationRecord> = self
            .state
            .lock()
            .unwrap()
            .automations
            .values()
            .cloned()
            .collect();
        let queue = self.log.limits().queue;
        let mut started = Vec::new();
        for mut a in list {
            if a.state.paused() || !self.log.claim(&a.id, &self.claim) {
                continue;
            }
            let before = (a.evaluated_seq, a.state);
            let mut full;
            // Read, then start the next run; a run started frees a place in the queue, so read
            // again until nothing moves.
            loop {
                let mut pending = self.pending_of(&a.id).len();
                full = false;
                let mut failed = false;
                'read: loop {
                    let batch = self.log.events_after(
                        &a.pattern.source,
                        &a.pattern.account,
                        a.evaluated_seq,
                        READ,
                    );
                    if batch.is_empty() {
                        break;
                    }
                    for e in batch {
                        if !self.current(&a, &e) || !a.pattern.matches(&e.event) {
                            a.evaluated_seq = e.seq;
                            continue;
                        }
                        if pending >= queue {
                            full = true;
                            break 'read;
                        }
                        let mut next = a.clone();
                        next.evaluated_seq = e.seq;
                        next.matched += 1;
                        next.last_event = Some(e.id.clone());
                        next.last_event_ms = Some(e.event.at_ms);
                        let ev = EventEvaluation::new(&next, &e);
                        if let Err(err) = self.persist(Some(&next), std::slice::from_ref(&ev)) {
                            tracing::warn!(automation = %a.id, "could not save a matched event: {err}");
                            failed = true;
                            break 'read;
                        }
                        a = next;
                        self.state
                            .lock()
                            .unwrap()
                            .evaluations
                            .insert(ev.id.clone(), ev);
                        pending += 1;
                    }
                }
                if failed {
                    break;
                }
                let run = self.submit_next(&mut a);
                let again = full && run.is_some();
                started.extend(run);
                if !again {
                    break;
                }
            }
            a.state = if full {
                AutomationState::Full
            } else {
                AutomationState::Listening
            };
            if (a.evaluated_seq, a.state) != before {
                if let Err(err) = self.persist(Some(&a), &[]) {
                    tracing::warn!(automation = %a.id, "could not save the automation: {err}");
                    continue;
                }
            }
            self.keep_automation(&a);
            if a.state != before.1 {
                self.changed_automation(&a);
            }
        }
        started
    }

    /// Whether `e` came after `a`'s current revision began: by arrival and by the provider's
    /// time, so a late re-delivery of an old event never runs under newer words.
    fn current(&self, a: &EventAutomationRecord, e: &StoredEvent) -> bool {
        e.seq > a.activated_seq && e.event.at_ms + CLOCK_SKEW_MS >= a.activated_ms
    }

    /// Start the run of `a`'s oldest waiting event when no run of it is open. A task made for
    /// that event before a crash is adopted, not made again (and not started twice: the
    /// restart already continues it).
    fn submit_next(&self, a: &mut EventAutomationRecord) -> Option<String> {
        loop {
            let open = {
                let st = self.state.lock().unwrap();
                st.tasks.values().any(|t| {
                    t.record.automation.as_deref() == Some(a.id.as_str())
                        && !t.record.state.is_final()
                })
            };
            if open {
                return None;
            }
            let next = self.pending_of(&a.id).into_iter().next()?;
            let task = run_task_id(&a.id, &next.event);
            let known = self
                .state
                .lock()
                .unwrap()
                .tasks
                .get(&task)
                .map(|t| t.record.state.is_final());
            let started = EventEvaluation {
                outcome: Outcome::Started { task: task.clone() },
                ..next.clone()
            };
            if let Some(ended) = known {
                a.last_task = Some(task.clone());
                if self
                    .persist(Some(a), std::slice::from_ref(&started))
                    .is_err()
                {
                    return None;
                }
                self.state
                    .lock()
                    .unwrap()
                    .evaluations
                    .insert(started.id.clone(), started);
                self.keep_automation(a);
                if ended {
                    continue;
                }
                return None;
            }
            let Some(event) = self.log.event(&next.event) else {
                tracing::warn!(automation = %a.id, event = %next.event, "a matched event is gone");
                let gone = EventEvaluation {
                    outcome: Outcome::Discarded,
                    ..next
                };
                if self.persist(None, std::slice::from_ref(&gone)).is_err() {
                    return None;
                }
                self.state
                    .lock()
                    .unwrap()
                    .evaluations
                    .insert(gone.id.clone(), gone);
                continue;
            };
            let mut spec = a.task.clone();
            let line = event_brief(&event, self.log.payload(&event.id).as_deref());
            spec.context = match spec.context.trim() {
                "" => line,
                c => format!("{c}\n{line}"),
            };
            spec.max_attempts = Some(1 + RETRIES);
            spec.retry_delay_ms = Some(crate::schedule::RETRY_DELAY_MS);
            let run = RunOf::Event {
                automation: &a.id,
                revision: a.revision,
                event: &event.id,
            };
            let id = match self.create_run(&a.agent, spec, Origin::External, None, Some(run)) {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!(automation = %a.id, "a run could not start: {e}");
                    return None;
                }
            };
            a.runs += 1;
            a.last_task = Some(id.clone());
            if let Err(e) = self.persist(Some(a), std::slice::from_ref(&started)) {
                // The task is saved: after a restart its evaluation adopts it.
                tracing::warn!(automation = %a.id, "could not save the started run: {e}");
            } else {
                self.state
                    .lock()
                    .unwrap()
                    .evaluations
                    .insert(started.id.clone(), started);
            }
            self.keep_automation(a);
            self.start(&id);
            return Some(id);
        }
    }

    /// An event run ended: count a failure in a row (3 pause the automation and say so in its
    /// chat), then start the next waiting event.
    pub(crate) fn event_run_ended(&self, run: &TaskRecord) {
        let Some(id) = &run.automation else { return };
        {
            let _one = self.evals.lock().unwrap();
            let found = self.state.lock().unwrap().automations.get(id).cloned();
            if let Some(mut a) = found.filter(|a| a.counted.as_deref() != Some(run.id.as_str())) {
                a.counted = Some(run.id.clone());
                match run.state {
                    TaskState::Failed => a.failures += 1,
                    TaskState::Done => a.failures = 0,
                    _ => {}
                }
                let mut cancelled = Cutting::default();
                let pause = a.failures >= super::MAX_FAILURES && !a.state.paused();
                if pause {
                    a.state = AutomationState::PausedFailing;
                    cancelled = self.cut(&a, Cut::Pause);
                }
                match self.persist(Some(&a), &cancelled.evals) {
                    Ok(()) => {
                        {
                            let mut st = self.state.lock().unwrap();
                            for e in cancelled.evals {
                                st.evaluations.insert(e.id.clone(), e);
                            }
                        }
                        self.keep_automation(&a);
                        self.cancel_unstarted(&cancelled.tasks);
                        if pause {
                            self.changed_automation(&a);
                            self.post_note(
                                &a.task,
                                &a.agent,
                                &run.id,
                                format!(
                                    "Paused after {} runs in a row did not finish. Ask to resume it when the cause is fixed.",
                                    a.failures
                                ),
                            );
                        }
                    }
                    Err(e) => tracing::warn!(automation = %a.id, "could not save a run's end: {e}"),
                }
            }
        }
        self.evaluate();
    }

    /// Tell the chat a task spec reports to something that is not a run's result.
    fn post_note(&self, task: &TaskSpec, agent: &str, run: &str, text: String) {
        let Some(chat) = &task.report_to else { return };
        let post = Post::new(agent, clip(&task.objective, 80), text, false);
        match ark_runtime::session::post(&self.engine, chat, post.clone()) {
            Ok(_) => self.emit(EventKind::Posted {
                session: chat.clone(),
                task: run.to_string(),
                post,
            }),
            Err(e) => tracing::warn!(agent, "could not post into the chat: {e}"),
        }
    }

    /// Load the project's automations and their evaluations.
    pub(crate) fn load_automations(&self, st: &mut State) -> Result<(), OrchError> {
        let store = &self.engine.store;
        for v in store.list_records(AUTOMATION, Some(self.project.id.as_str()))? {
            if let Ok(a) = serde_json::from_value::<EventAutomationRecord>(v) {
                st.automations.insert(a.id.clone(), a);
            }
        }
        for v in store.list_records(EVALUATION, Some(self.project.id.as_str()))? {
            if let Ok(e) = serde_json::from_value::<EventEvaluation>(v) {
                st.evaluations.insert(e.id.clone(), e);
            }
        }
        Ok(())
    }

    /// Whether `agent` is an automation's: it runs nothing else.
    pub(crate) fn runs_an_automation(&self, st: &State, agent: &str) -> bool {
        st.automations.values().any(|a| a.agent == agent)
    }
}
