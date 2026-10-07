//! A task on an agent program (ADR 0039, ADR 0043). The program does the work with its own
//! tools; the task stays this orchestrator's: its questions go through the task's gate (kept,
//! answered from any client, never asked twice), its usage and steps arrive as the harness's
//! events do, and what it changed is found in the folder and recorded as one call, so outputs,
//! versions before and the trace work as for ARK's own agents.

use std::path::Path;
use std::sync::Arc;

use ark_core::{Invocation, ProjectId, RiskTier};
use ark_model::{Completion, Message, Provider, ToolDef};
use ark_runtime::folder::Before;
use ark_runtime::harness::{FileBefore, HarnessEvent};
use ark_runtime::worker::{parse_spec, Worker, WorkerJob};
use ark_runtime::{ApprovalDecision, ApprovalGate, ApprovalRequest, CancelToken, ModelRouter};
use ark_tools::ProducedArtifact;
use serde_json::json;

use super::{Attempt, Inner, OnBehalf, Stop};
use crate::models::{AgentModels, ModelResolver};
use crate::record::Decision;
use crate::OrchError;

/// What an agent program's run is kept under: the store record of what it keeps for a later
/// run (its session, its process), by task.
const KEPT: &str = "orch_worker";
/// The action of the call that records a run's work.
const WORK: &str = "program.files";
/// The most written files kept as a run's outputs, and the largest one.
const MAX_OUTPUTS: usize = 50;
const MAX_OUTPUT_BYTES: u64 = 25 * 1024 * 1024;

/// An agent on a program has no model of ARK's: its slots are the program's.
pub(super) fn models(
    w: &dyn Worker,
    spec: Option<&str>,
    resolver: &dyn ModelResolver,
) -> AgentModels {
    let spec = spec.unwrap_or_default().to_string();
    // Its slots are the program's account's; one task per project folder at a time is the
    // orchestrator's (`Inner::folders`).
    let capacity = resolver.capacity_of(w.name());
    AgentModels {
        router: ModelRouter::single(Arc::new(Elsewhere(spec.clone()))),
        account: w.name().to_string(),
        capacity,
        model: spec,
        session: None,
    }
}

/// The model slot of an agent that runs on a program: never called.
struct Elsewhere(String);

#[async_trait::async_trait]
impl Provider for Elsewhere {
    async fn complete(
        &self,
        _: &str,
        _: &[Message],
        _: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        Err(ark_model::ProviderError::Api {
            provider: self.0.clone(),
            detail: "it runs on its own program, not on a model".into(),
        })
    }
    fn describe(&self) -> String {
        self.0.clone()
    }
}

impl Inner {
    /// The program an agent's model spec names, if it names one.
    pub(super) fn worker_for(
        &self,
        spec: Option<&str>,
    ) -> Result<Option<Arc<dyn Worker>>, OrchError> {
        let Some((name, _)) = spec.and_then(parse_spec) else {
            return Ok(None);
        };
        match self.workers.as_ref().and_then(|w| w.worker(name)) {
            Some(Ok(w)) => Ok(Some(w)),
            Some(Err(e)) => Err(OrchError::NotAllowed(e)),
            None => Err(OrchError::NotAllowed(format!(
                "`agent:{name}` names nothing this app can run"
            ))),
        }
    }

    /// Run task `id` on `worker`.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_worker(
        self: &Arc<Self>,
        id: &str,
        owner: &str,
        worker: Arc<dyn Worker>,
        scope: &ProjectId,
        brief: &str,
        cancel: &CancelToken,
        allow_mutate: bool,
        spec: &str,
    ) -> Attempt {
        let folder = match self.engine.workspace_dir(scope) {
            Ok(d) => d,
            Err(e) => return Attempt::Failed(e.to_string()),
        };
        // One program at a time in a folder: what changes there is this task's.
        let lock = {
            let mut f = self.folders.lock().unwrap();
            f.entry(folder.clone()).or_default().clone()
        };
        let _one = tokio::select! {
            g = lock.lock_owned() => g,
            _ = cancel.cancelled() => return Attempt::Stopped(self.stopped(id)),
        };
        let dir = self.work_dir(id);
        if let Err(e) = self.folder_before(&folder, &dir).await {
            return Attempt::Failed(e);
        }
        // An earlier run of this task in a process that is gone: stop what it left running.
        let kept = self.engine.store.get_record(KEPT, id).ok().flatten();
        if let Some(k) = &kept {
            worker.stop_left(k);
        }
        let approvals = self.approvals.as_ref().map(|gate| {
            Arc::new(ApprovalGate::new(Arc::new(OnBehalf {
                orch: self.clone(),
                agent: owner.to_string(),
                task: id.to_string(),
                gate: gate.clone(),
            })))
        });
        let store = self.engine.store.clone();
        let (tid, project) = (id.to_string(), self.project.id.to_string());
        let keep = move |v: serde_json::Value| {
            if let Err(e) = store.put_record(KEPT, &tid, &project, &v) {
                tracing::warn!(task = %tid, "could not keep the run's state: {e}");
            }
        };
        let me = self.clone();
        let (tid, aname) = (id.to_string(), owner.to_string());
        let on_event = move |ev: HarnessEvent| me.on_agent_event(&tid, &aname, ev);
        let model = parse_spec(spec).and_then(|(_, m)| m);
        let started = std::time::Instant::now();
        let result = worker
            .run(WorkerJob {
                brief,
                folder: &folder,
                model,
                approvals,
                allow_mutate,
                cancel: cancel.clone(),
                kept,
                keep: &keep,
                on_event: &on_event,
            })
            .await;
        self.update_task(id, |t| {
            t.run_ms += started.elapsed().as_millis() as u64;
            if let Ok(r) = &result {
                t.usage.cost_micro_usd += r.cost_micro_usd.unwrap_or(0);
            }
        });
        // The folder state and the program's are kept until the task ends ([`Inner::end_work`]):
        // a restart, a pause or a retry continues against the same start.
        match result {
            Ok(r) if !r.cancelled => Attempt::Done(r.answer),
            Ok(_) => Attempt::Stopped(self.stopped(id)),
            Err(_) if cancel.is_cancelled() => Attempt::Stopped(self.stopped(id)),
            Err(e) => Attempt::Failed(e),
        }
    }

    fn work_dir(&self, id: &str) -> std::path::PathBuf {
        self.engine.store.root().join("work").join(id)
    }

    /// The task ended, however it ended: stop what its program left running, record what it
    /// changed in the folder (the task's outputs, versions before for Restore), and remove the
    /// folder state. Nothing for a task that never ran on a program.
    pub(super) async fn end_work(self: &Arc<Self>, id: &str) {
        let dir = self.work_dir(id);
        let Some(before) = Before::load(&dir.join("before.json")) else {
            return;
        };
        let (owner, scope, worker) = {
            let st = self.state.lock().unwrap();
            let Some(t) = st.tasks.get(id) else { return };
            let a = st.agents.get(&t.record.owner);
            (
                t.record.owner.clone(),
                a.map(|a| ProjectId(a.record.scope.clone())),
                a.and_then(|a| a.worker.clone()),
            )
        };
        if let (Some(w), Ok(Some(k))) = (&worker, self.engine.store.get_record(KEPT, id)) {
            w.stop_left(&k);
        }
        if let Some(scope) = scope {
            if let Ok(folder) = self.engine.workspace_dir(&scope) {
                let me = self.clone();
                let (tid, aname) = (id.to_string(), owner.clone());
                let on_event = move |ev: HarnessEvent| me.on_agent_event(&tid, &aname, ev);
                self.record_work(id, &owner, &scope, &folder, before, &on_event)
                    .await;
            }
        }
        let _ = self.engine.store.delete_record(KEPT, id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn stopped(&self, id: &str) -> Stop {
        self.stop_reason(id)
            .unwrap_or(if self.engine.is_stopping() {
                Stop::Restart
            } else {
                Stop::Cancel
            })
    }

    /// The folder as it was when the task first started: taken then, read back in later runs.
    async fn folder_before(&self, folder: &Path, dir: &Path) -> Result<(), String> {
        if dir.join("before.json").is_file() {
            return Ok(());
        }
        let (folder, dir) = (folder.to_path_buf(), dir.to_path_buf());
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            Before::take(&folder, &dir.join("before"))
                .save(&dir.join("before.json"))
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// What the program changed in the folder, as one call of the task: the files it wrote
    /// (the task's outputs) and the versions before of what it changed or removed.
    async fn record_work(
        &self,
        id: &str,
        owner: &str,
        scope: &ProjectId,
        folder: &Path,
        before: Before,
        on_event: &(dyn Fn(HarnessEvent) + Send + Sync),
    ) {
        let root = folder.to_path_buf();
        let Ok(kept) = tokio::task::spawn_blocking(move || {
            let kept = before.changed(&root);
            let mut produced: Vec<ProducedArtifact> = Vec::new();
            for rel in kept.written.iter().take(MAX_OUTPUTS) {
                let p = root.join(rel);
                if p.metadata().is_ok_and(|m| m.len() <= MAX_OUTPUT_BYTES) {
                    if let Ok(bytes) = std::fs::read(&p) {
                        let name = rel.rsplit('/').next().unwrap_or(rel);
                        produced.push(
                            ProducedArtifact::bytes(ark_core::ArtifactKind::Blob, name, bytes)
                                .at(rel.as_str()),
                        );
                    }
                }
            }
            (kept, produced)
        })
        .await
        else {
            return;
        };
        let (kept, mut produced) = kept;
        if kept.written.is_empty() && kept.removed.is_empty() {
            return;
        }
        let befores: Vec<String> = kept
            .versions
            .iter()
            .filter_map(|v| v.path.clone())
            .collect();
        produced.extend(kept.versions);
        let mut inv = Invocation::new(
            WORK,
            RiskTier::Mutate,
            json!({ "task": id, "agent": owner }),
        );
        if let Err(e) = self.engine.keep_produced(scope, &mut inv, produced) {
            tracing::warn!(task = %id, "could not keep what the program wrote: {e}");
        }
        inv.mark_ok(json!({
            "written": kept.written,
            "removed": kept.removed,
            "no_copy": kept.no_copy,
        }));
        if let Err(e) = self.engine.store.put_invocation(scope, &inv) {
            tracing::warn!(task = %id, "could not record the work: {e}");
            return;
        }
        // The files' versions before, by path, for a front-end's Restore.
        let mut before = Vec::new();
        for a in &inv.produced {
            if let Ok(Some(art)) = self.engine.store.resolve_artifact(scope, a.as_str()) {
                if art.meta["before"] == true {
                    if let Some(p) = art.meta["path"]
                        .as_str()
                        .filter(|p| befores.iter().any(|b| b == p))
                    {
                        before.push(FileBefore {
                            path: p.to_string(),
                            artifact: art.id.to_string(),
                        });
                    }
                }
            }
        }
        let call = format!("work-{}", inv.id.as_str());
        on_event(HarnessEvent::ToolCall {
            id: call.clone(),
            action: WORK.into(),
            input: json!({}),
        });
        let summary = match (kept.written.len(), kept.removed.len()) {
            (w, 0) => format!("wrote {w} file{}", if w == 1 { "" } else { "s" }),
            (0, r) => format!("removed {r} file{}", if r == 1 { "" } else { "s" }),
            (w, r) => format!("wrote {w}, removed {r}"),
        };
        on_event(HarnessEvent::ToolResult {
            id: call,
            action: WORK.into(),
            ok: true,
            summary,
            invocation: inv.id.as_str().to_string(),
            duration_ms: None,
            error: None,
            context_tokens: 0,
            raw_tokens: 0,
            files: kept.written,
            before,
        });
        self.collect_outputs(id, owner, scope);
    }

    /// Keep an answer the person gave in this task, for the run that continues it after the
    /// process stops.
    pub(super) fn decided(&self, task: &str, r: &ApprovalRequest, d: ApprovalDecision) {
        if r.ask_each_time {
            return;
        }
        let key = Decision::key_of(&r.action, &r.input);
        self.update_task(task, |t| {
            let attempt = t.attempts;
            t.decisions.retain(|x| x.key != key);
            t.decisions.push(Decision {
                key,
                decision: d,
                attempt,
                carry: false,
            });
        });
    }

    /// The answer the person gave for the same call in the run the process stopped in. Only a
    /// refusal, or leave to change a file (writing the same file again changes nothing), carries
    /// over: a command may have run already, so the person decides again. Within one run, "once"
    /// stays once.
    pub(super) fn decided_before(
        &self,
        task: &str,
        r: &ApprovalRequest,
    ) -> Option<ApprovalDecision> {
        if r.ask_each_time {
            return None;
        }
        let key = Decision::key_of(&r.action, &r.input);
        let st = self.state.lock().unwrap();
        let t = &st.tasks.get(task)?.record;
        t.decisions
            .iter()
            .find(|x| x.carry && x.key == key)
            .map(|x| x.decision)
            .filter(|d| !d.allows() || r.action == ark_runtime::worker::CHANGE)
    }
}
