//! Event automations on the orchestrator (ADR 0062): create, pause, resume, change, discard and
//! stop, the evaluator that turns saved events into runs, and what happens when a run ends. The
//! rules are in [`crate::automation`].

use super::*;
use crate::authority::AutomationAuthority;
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
    pub authority: Option<AutomationAuthority>,
}

/// What a cutover ends: matched events still waiting get a final outcome, and a run made but
/// not begun is cancelled once that is saved.
#[derive(Default)]
struct Cutting {
    evals: Vec<EventEvaluation>,
    tasks: Vec<String>,
    /// Those tasks as saved, cancelled, in the cutover's transaction: a crash before they end
    /// never runs them under the old words.
    records: Vec<TaskRecord>,
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
        authority: AutomationAuthority,
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
            if !a.record.spec.delegates_to.is_empty() {
                return Err(OrchError::NotAllowed(format!(
                    "`{agent}` hands work to other agents; an automation's agent keeps each event to itself"
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
            authority,
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
            limited: None,
        };
        self.inner.persist(Some(&rec), &[])?;
        self.inner.keep_automation(&rec);
        self.inner.changed_automation(&rec);
        Ok(rec)
    }

    /// The store's event log, where providers deliver events and apps offer accounts.
    pub fn event_log(&self) -> Arc<crate::automation::EventLog> {
        self.inner.log.clone()
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
            if let Some(au) = &change.authority {
                a.authority = au.clone();
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

    /// Leave a gap in the feed of an automation's account the way the person chose
    /// ([`crate::Recovery`]). Every automation on that account reads on from there.
    pub async fn recover_feed(
        &self,
        id: &str,
        how: crate::Recovery,
    ) -> Result<crate::FeedRecord, OrchError> {
        let a = self
            .automation(id)
            .ok_or_else(|| self.inner.no_automation(id))?;
        let feed = self
            .inner
            .log
            .feed(&a.pattern.source, &a.pattern.account)
            .ok_or_else(|| {
                OrchError::NotAllowed(format!(
                    "{} on {} is not connected",
                    a.pattern.account, a.pattern.source
                ))
            })?;
        let r = feed.recover(how).await.map_err(OrchError::NotAllowed)?;
        for x in self.automations() {
            if x.pattern.scope() == a.pattern.scope() {
                self.inner.changed_automation(&x);
            }
        }
        Ok(r)
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
        self.inner
            .persist_with(None, &cancelled.evals, &cancelled.records)?;
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
        self.inner.log.ensure_feeds();
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
        let mut moved = self.inner.log.feeds_moved();
        tokio::spawn(async move {
            let mut tidy = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                {
                    let Some(inner) = weak.upgrade() else { break };
                    inner.evaluate();
                }
                tokio::select! {
                    _ = tidy.tick() => {
                        let Some(inner) = weak.upgrade() else { break };
                        inner.log.prune(crate::record::now_ms());
                    }
                    r = grown.changed() => if r.is_err() { break },
                    r = moved.changed() => {
                        if r.is_err() {
                            break;
                        }
                        // A feed's state shows on every automation of its account.
                        let Some(inner) = weak.upgrade() else { break };
                        let all: Vec<EventAutomationRecord> =
                            inner.state.lock().unwrap().automations.values().cloned().collect();
                        for a in &all {
                            inner.changed_automation(a);
                        }
                    }
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
        self.persist_with(a, evals, &[])
    }

    /// [`persist`](Self::persist), with task records in the same transaction.
    fn persist_with(
        &self,
        a: Option<&EventAutomationRecord>,
        evals: &[EventEvaluation],
        tasks: &[TaskRecord],
    ) -> Result<(), OrchError> {
        let project = self.project.id.as_str();
        let mut values = Vec::new();
        for t in tasks {
            values.push((TASK, t.id.clone(), serde_json::to_value(t)?));
        }
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
        self.log.ensure_feeds();
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
        self.persist_with(Some(&a), &cancelled.evals, &cancelled.records)?;
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
        let (out, unstarted, records) = {
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
            let mut out = Vec::new();
            for e in st.evaluations.values().filter(|e| e.automation == a.id) {
                let task = match &e.outcome {
                    Outcome::Pending => run_task_id(&a.id, &e.event),
                    Outcome::Started { task } => task.clone(),
                    _ => continue,
                };
                let outcome = match st.tasks.get(&task) {
                    // Its run began (before a crash took its mark): it goes on, as runs do.
                    Some(_) if !unstarted.contains(&task) => Outcome::Started { task },
                    _ => how.outcome(),
                };
                if outcome != e.outcome {
                    out.push(EventEvaluation {
                        outcome,
                        ..e.clone()
                    });
                }
            }
            let records = unstarted
                .iter()
                .filter_map(|t| st.tasks.get(t))
                .map(|t| {
                    let mut r = t.record.clone();
                    r.state = TaskState::Cancelled;
                    r.waiting_on = None;
                    r.finished_ms = Some(now_ms());
                    r
                })
                .collect();
            (out, unstarted, records)
        };
        Cutting {
            evals: out,
            tasks: unstarted,
            records,
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

    /// Mark started the waiting events whose task exists already (made before a crash took
    /// the mark): never made again, and not counted as waiting.
    fn adopt_made(&self, a: &mut EventAutomationRecord) {
        let made: Vec<EventEvaluation> = {
            let st = self.state.lock().unwrap();
            st.evaluations
                .values()
                .filter(|e| e.automation == a.id && e.outcome == Outcome::Pending)
                .filter_map(|e| {
                    let task = run_task_id(&a.id, &e.event);
                    st.tasks.contains_key(&task).then(|| EventEvaluation {
                        outcome: Outcome::Started { task },
                        ..e.clone()
                    })
                })
                .collect()
        };
        if made.is_empty() || self.persist(None, &made).is_err() {
            return;
        }
        let mut st = self.state.lock().unwrap();
        for e in made {
            if let Outcome::Started { task } = &e.outcome {
                a.last_task.get_or_insert_with(|| task.clone());
            }
            st.evaluations.insert(e.id.clone(), e);
        }
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
        e.seq > a.activated_seq && e.event.at_ms.saturating_add(CLOCK_SKEW_MS) >= a.activated_ms
    }

    /// Start the run of `a`'s oldest waiting event when no run of it is open. A task made for
    /// that event before a crash is adopted, not made again (and not started twice: the
    /// restart already continues it).
    fn submit_next(&self, a: &mut EventAutomationRecord) -> Option<String> {
        self.adopt_made(a);
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
                authority: &a.authority,
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

/// Changes allowed under an authority are counted under one lock, so two runs never both take
/// the last one of a quota.
pub(crate) const QUOTA: &str = "automation_quota";

/// An event run's authority as the harness checks it: before every call, then each change is
/// counted (per run, and against its quota) before it runs, and an uncertain one stops the run
/// from changing anything else.
struct AuthorityGuard {
    inner: Arc<Inner>,
    task: String,
    automation: String,
    authority: AutomationAuthority,
    event: StoredEvent,
    workspace: Option<std::path::PathBuf>,
}

impl Inner {
    /// The guard of event run `task`. Errs when its event is gone: then it cannot be bound.
    pub(crate) fn guard_for(
        &self,
        task: &TaskRecord,
        authority: &AutomationAuthority,
    ) -> Result<Arc<dyn ark_runtime::CallGuard>, String> {
        let event = task
            .event
            .as_deref()
            .and_then(|e| self.log.event(e))
            .ok_or("the event this run is for is gone")?;
        Ok(Arc::new(AuthorityGuard {
            inner: self.arc(),
            task: task.id.clone(),
            automation: task.automation.clone().unwrap_or_default(),
            authority: authority.clone(),
            event,
            workspace: self.engine.workspace_dir(&self.project.id).ok(),
        }))
    }
}

impl ark_runtime::CallGuard for AuthorityGuard {
    fn check(
        &self,
        action: &str,
        input: &serde_json::Value,
        risk: Option<ark_core::RiskTier>,
    ) -> ark_runtime::Verdict {
        self.authority
            .check(action, input, risk, &self.event, self.workspace.as_deref())
    }

    fn commit(&self, action: &str, _input: &serde_json::Value) -> Result<(), String> {
        let allowed = self
            .authority
            .allowed(action)
            .ok_or_else(|| format!("`{action}` is not one of the actions it may use"))?;
        let inner = &self.inner;
        let _one = inner.quotas.lock().unwrap();
        let mut task = inner
            .state
            .lock()
            .unwrap()
            .tasks
            .get(&self.task)
            .map(|t| t.record.clone())
            .ok_or("the run is gone")?;
        if task.effect_uncertain {
            return Err(
                "an earlier change in this run may or may not have happened; nothing more changes until the person checks"
                    .into(),
            );
        }
        let used = task.authority_used.get(action).copied().unwrap_or(0);
        if used >= allowed.per_run {
            return Err(format!(
                "`{action}` may run {} time{} per event",
                allowed.per_run,
                if allowed.per_run == 1 { "" } else { "s" }
            ));
        }
        let project = inner.project.id.to_string();
        let mut quota = None;
        if let Some(q) = allowed.quota {
            let now = now_ms();
            let start = now - now % q.window_ms.max(1);
            let id = format!("{}/{action}/{start}", self.automation);
            let count = inner
                .engine
                .store
                .get_record(QUOTA, &id)
                .ok()
                .flatten()
                .and_then(|v| v["count"].as_u64())
                .unwrap_or(0);
            if count >= q.count as u64 {
                let until = start + q.window_ms;
                inner.rate_limited(&self.automation, action, until);
                return Err(format!(
                    "rate limited: `{action}` may run {} times per {}; it can run again at {}",
                    q.count,
                    window_words(q.window_ms),
                    crate::schedule::local_label(until)
                ));
            }
            quota = Some((
                id,
                serde_json::json!({ "count": count + 1, "until_ms": start + q.window_ms }),
            ));
        }
        *task.authority_used.entry(action.to_string()).or_default() += 1;
        let task_json = serde_json::to_value(&task).map_err(|e| e.to_string())?;
        let mut rows: Vec<(&str, &str, &str, &serde_json::Value)> =
            vec![(TASK, task.id.as_str(), project.as_str(), &task_json)];
        if let Some((id, v)) = &quota {
            rows.push((QUOTA, id.as_str(), project.as_str(), v));
        }
        inner
            .engine
            .store
            .put_records(&rows)
            .map_err(|e| format!("could not record the change before making it: {e}"))?;
        if let Some(t) = inner.state.lock().unwrap().tasks.get_mut(&self.task) {
            t.record.authority_used = task.authority_used;
        }
        Ok(())
    }

    fn ended(&self, _action: &str, risk: ark_core::RiskTier, _ok: bool, uncertain: bool) {
        if risk == ark_core::RiskTier::Mutate && uncertain {
            self.inner.update_task(&self.task, |t| {
                t.effect_uncertain = true;
                // Trying again could make the change twice.
                t.max_attempts = t.attempts.max(1);
            });
        }
    }
}

impl Inner {
    /// Note on the automation that a change was refused for its quota until `until_ms`.
    fn rate_limited(&self, automation: &str, action: &str, until_ms: u64) {
        let limited = crate::automation::RateLimited {
            action: action.to_string(),
            until_ms,
        };
        let _ = self.alter(automation, |_, a| {
            a.limited = Some(limited);
            Ok(Cutting::default())
        });
    }
}

fn window_words(ms: u64) -> String {
    match ms {
        86_400_000 => "day".into(),
        3_600_000 => "hour".into(),
        m if m % 86_400_000 == 0 => format!("{} days", m / 86_400_000),
        m if m % 3_600_000 == 0 => format!("{} hours", m / 3_600_000),
        m => format!("{} minutes", (m / 60_000).max(1)),
    }
}
