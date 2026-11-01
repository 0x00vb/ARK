use super::*;
use crate::automation::{self, Bound, InputValue, Run};
use ark_core::{ActionDescriptor, Invocation, InvocationStatus, RiskTier};
use ark_runtime::guard::{CallGuard, Verdict};
use ark_store::RecordTransaction;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const EFFECT: &str = "automation_effect";
const META_CALLS: &str = "automation_meta_calls";

#[derive(Serialize, Deserialize)]
struct MetaCalls {
    calls: u64,
}

fn meta_calls(tx: &RecordTransaction<'_>, run: &str) -> Result<u64, OrchError> {
    Ok(tx
        .get_record(META_CALLS, run)?
        .map(serde_json::from_value::<MetaCalls>)
        .transpose()?
        .map_or(0, |value| value.calls))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    Running,
    Confirmed,
    Uncertain,
    Abandoned,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct QuotaReservation {
    key: String,
    start: u64,
    window_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Effect {
    run: String,
    invocation: String,
    project: String,
    action: String,
    digest: String,
    risk: RiskTier,
    phase: Phase,
    at_ms: u64,
    quotas: Vec<QuotaReservation>,
}

#[derive(Default)]
struct Live {
    active: HashSet<String>,
    error: Option<String>,
}

pub(super) struct RunGuard {
    engine: Arc<Engine>,
    project: String,
    run: String,
    token: String,
    general: bool,
    live: Arc<Mutex<Live>>,
}

fn deny(reason: impl Into<String>) -> OrchError {
    OrchError::NotAllowed(reason.into())
}
fn identity(run: &str, invocation: &str) -> String {
    format!(
        "ae-{}",
        blake3::hash(format!("{run}/{invocation}").as_bytes()).to_hex()
    )
}
fn digest(invocation: &Invocation) -> Result<String, OrchError> {
    Ok(blake3::hash(&serde_json::to_vec(&(
        &invocation.action,
        &invocation.input,
        &invocation.risk,
    ))?)
    .to_hex()
    .to_string())
}
fn fingerprint(descriptor: &ActionDescriptor) -> Result<serde_json::Value, OrchError> {
    Ok(serde_json::to_value((
        &descriptor.name,
        &descriptor.risk,
        &descriptor.input_schema,
        &descriptor.ask_each_time,
        &descriptor.person_only,
        &descriptor.auto_eligible,
        &descriptor.grant,
    ))?)
}

impl Inner {
    pub(super) fn run_guard(&self, run: &str, general: bool) -> Result<Arc<RunGuard>, OrchError> {
        type Key = (std::path::PathBuf, String, String);
        type Lives = Mutex<HashMap<Key, Weak<Mutex<Live>>>>;
        static LIVES: std::sync::OnceLock<Lives> = std::sync::OnceLock::new();
        let root =
            std::fs::canonicalize(self.engine.store.root()).map_err(ark_store::StoreError::from)?;
        let mut lives = LIVES.get_or_init(Default::default).lock().unwrap();
        let key = (root, self.project.id.to_string(), run.to_string());
        let live = match lives.get(&key).and_then(Weak::upgrade) {
            Some(live) => live,
            None => {
                lives.retain(|_, live| live.strong_count() > 0);
                let live = Arc::new(Mutex::new(Live::default()));
                lives.insert(key, Arc::downgrade(&live));
                live
            }
        };
        Ok(Arc::new(RunGuard {
            engine: self.engine.clone(),
            project: self.project.id.to_string(),
            run: run.into(),
            token: self.execution_token.clone(),
            general,
            live,
        }))
    }
}

impl RunGuard {
    pub(super) fn ensure_current(&self) -> Result<(), OrchError> {
        let live = self.live.lock().unwrap();
        if let Some(error) = &live.error {
            return Err(deny(error.clone()));
        }
        self.engine.store.transact_records(&self.project, |tx| {
            self.load_in(tx, true)?;
            for value in tx.records_matching(EFFECT, "run", &self.run)? {
                let mut effect: Effect = serde_json::from_value(value)?;
                if matches!(effect.phase, Phase::Prepared | Phase::Running) && !live.active.contains(&effect.invocation) {
                    let call = tx.execution_invocation(&effect.project, &effect.invocation)?;
                    if let Some(call) = &call {
                        if digest(call)? != effect.digest { return Err(deny("the retained effect changed identity")); }
                        if matches!(call.status, InvocationStatus::Ok | InvocationStatus::Failed) {
                            effect.phase = Self::finished_phase(call);
                            tx.put_record(EFFECT, &identity(&self.run, &effect.invocation), &serde_json::to_value(&effect)?)?;
                        } else if effect.phase == Phase::Prepared && call.status == InvocationStatus::PendingApproval {
                            continue;
                        }
                    }
                }
                if effect.risk == RiskTier::Mutate && (effect.phase == Phase::Uncertain || matches!(effect.phase, Phase::Prepared | Phase::Running) && !live.active.contains(&effect.invocation)) {
                    return Err(deny("an earlier change is unconfirmed; review its recorded result before continuing"));
                }
            }
            Ok(())
        })
    }

    fn load(&self, tx: &RecordTransaction<'_>) -> Result<Run, OrchError> {
        self.load_in(tx, false)
    }

    fn load_in(&self, tx: &RecordTransaction<'_>, _waiting: bool) -> Result<Run, OrchError> {
        let run: Run = serde_json::from_value(
            tx.get_record(automation::RUN, &self.run)?
                .ok_or_else(|| deny("the execution is unavailable"))?,
        )?;
        if run.project != self.project
            || run.id != self.run
            || run.cancel_requested
            || !matches!(
                run.phase,
                automation::RunPhase::Running | automation::RunPhase::WaitingApproval
            )
            || run
                .owner
                .as_ref()
                .is_none_or(|owner| owner.token != self.token)
        {
            return Err(deny("this execution no longer admits actions"));
        }
        if run.revision.computed_digest()? != run.revision.digest {
            return Err(deny("the frozen revision changed"));
        }
        if let Some(id) = &run.activation {
            let activation: automations::PreparedActivation = serde_json::from_value(
                tx.get_record(automations::ACTIVATION, id)?
                    .ok_or_else(|| deny("the activation grant is unavailable"))?,
            )?;
            if activation.value.phase != automation::ActivationPhase::Active
                || activation.value.automation != run.automation
                || activation.value.revision != run.revision.number
            {
                return Err(deny("this execution's grant was revoked"));
            }
            let frozen = serde_json::to_value((&run.selections, &run.agents, &run.descriptors))?;
            let grant = serde_json::to_value((
                &activation.value.selections,
                &activation.value.agents,
                &activation.value.descriptors,
            ))?;
            let expected = activation.value.computed_digest(&run.revision.digest)?;
            if let Some(webhook) = &activation.value.webhook {
                if !matches!(&run.origin,automation::RunOrigin::Webhook { endpoint,account,source,.. } if endpoint==&webhook.endpoint && account==&webhook.account && source=="webhook")
                {
                    return Err(deny("the execution does not match its webhook authority"));
                }
            }
            if frozen != grant || expected != activation.value.digest {
                return Err(deny("the execution no longer matches its frozen grant"));
            }
        }
        Ok(run)
    }

    fn descriptor<'a>(
        &self,
        run: &'a Run,
        action: &str,
    ) -> Result<&'a ActionDescriptor, OrchError> {
        let general = format!("general:{action}");
        (if self.general {
            run.descriptors
                .get(&general)
                .or_else(|| run.descriptors.get(action))
        } else {
            run.descriptors.get(action)
        })
        .ok_or_else(|| deny("the action has no frozen descriptor"))
    }

    fn allowed<'a>(
        &self,
        run: &'a Run,
        action: &str,
        input: &serde_json::Value,
        actual: &ActionDescriptor,
    ) -> Result<&'a automation::AllowedAction, OrchError> {
        let allowed = run
            .revision
            .draft
            .authority
            .actions
            .iter()
            .find(|allowed| allowed.action == action)
            .ok_or_else(|| deny(format!("{action} is outside this execution's authority")))?;
        if action.starts_with("agents.") && !matches!(action, tools::DELEGATE | tools::WAIT) {
            return Err(deny(
                "this agent operation cannot preserve the execution's authority",
            ));
        }
        if fingerprint(self.descriptor(run, action)?)? != fingerprint(actual)? || actual.person_only
        {
            return Err(deny("the action changed since approval"));
        }
        let fields = input
            .as_object()
            .ok_or_else(|| deny("action arguments must be an object"))?;
        if fields
            .keys()
            .any(|field| !allowed.inputs.contains_key(field))
        {
            return Err(deny("the action contains an unapproved input"));
        }
        for (name, bound) in &allowed.inputs {
            let given = fields.get(name);
            let matches = match bound {
                Bound::Free => true,
                Bound::Literal { value } => given == Some(value),
                Bound::Input { name } => match (run.inputs.get(name), given) {
                    (Some(InputValue::Text { value }), Some(serde_json::Value::String(given))) => {
                        given == value
                    }
                    (Some(InputValue::Files { ids }), Some(serde_json::Value::Array(given))) => {
                        given
                            .iter()
                            .map(serde_json::Value::as_str)
                            .eq(ids.iter().map(|id| Some(id.as_str())))
                    }
                    _ => false,
                },
                Bound::WorkspacePath { .. } => {
                    return Err(deny(
                        "the requested folder bound has no verified containment contract",
                    ))
                }
            };
            if !matches {
                return Err(deny(format!(
                    "{name} does not match the frozen input bound"
                )));
            }
        }
        if action == tools::DELEGATE {
            let to = fields
                .get("to")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| deny("delegation needs an approved agent"))?;
            if !run.agents.contains_key(to) {
                return Err(deny(
                    "the delegation target was not approved for this execution",
                ));
            }
        }
        Ok(allowed)
    }

    fn require_scope(
        &self,
        tx: &RecordTransaction<'_>,
        run: &Run,
        project: &str,
    ) -> Result<(), OrchError> {
        for step in &run.steps {
            if step.phase != automation::StepPhase::Running {
                continue;
            }
            if step.scope == project && step.receipt.is_none() {
                return Ok(());
            }
            for value in tx.records_matching(TASK, "execution_scope", project)? {
                let task: TaskRecord = serde_json::from_value(value)?;
                if task.run.as_deref() == Some(&run.id)
                    && task.execution_step.as_deref() == Some(&step.id)
                    && !task.state.is_final()
                    && task.execution_owner.as_deref() == Some(&self.token)
                {
                    return Ok(());
                }
            }
        }
        Err(deny(
            "this action belongs to a step that is no longer running",
        ))
    }

    fn finished_phase(invocation: &Invocation) -> Phase {
        if invocation.risk == RiskTier::Mutate
            && (invocation.status != InvocationStatus::Ok
                || invocation.effect.as_ref().is_some_and(|effect| {
                    effect.state == ark_core::ExternalEffectState::Unconfirmed
                }))
        {
            Phase::Uncertain
        } else {
            Phase::Confirmed
        }
    }

    fn update(
        &self,
        project: &ProjectId,
        invocation: &Invocation,
        phase: Phase,
        starting: bool,
    ) -> Result<(), String> {
        let mut live = self.live.lock().unwrap();
        let result = self
            .engine
            .store
            .transact_records::<_, OrchError>(&self.project, |tx| {
                let run = if starting { Some(self.load(tx)?) } else { None };
                if let Some(run) = &run {
                    self.require_scope(tx, run, project.as_str())?;
                }
                let key = identity(&self.run, invocation.id.as_str());
                let Some(value) = tx.get_record(EFFECT, &key)? else {
                    if !starting && phase == Phase::Abandoned {
                        return Ok(());
                    }
                    return Err(deny("the effect reservation is unavailable"));
                };
                let mut effect: Effect = serde_json::from_value(value)?;
                if effect.project != project.as_str() || effect.digest != digest(invocation)? {
                    return Err(deny("the effect reservation names another call"));
                }
                if effect.phase == Phase::Confirmed {
                    return Ok(());
                }
                if let Some(run) = &run {
                    if effect.phase != Phase::Prepared
                        || !live.active.contains(invocation.id.as_str())
                    {
                        return Err(deny("this effect was already started or interrupted"));
                    }
                    self.allowed(
                        run,
                        &invocation.action,
                        &invocation.input,
                        self.descriptor(run, &invocation.action)?,
                    )?;
                    let stored = tx
                        .execution_invocation(project.as_str(), invocation.id.as_str())?
                        .ok_or_else(|| deny("the running invocation is unavailable"))?;
                    if stored.status != InvocationStatus::Running
                        || digest(&stored)? != effect.digest
                    {
                        return Err(deny("the running invocation changed"));
                    }
                    for quota in &effect.quotas {
                        if now_ms() / quota.window_ms * quota.window_ms != quota.start
                            || tx
                                .quota_used(&quota.key, quota.start)?
                                .is_none_or(|used| used == 0)
                        {
                            return Err(deny("the effect's quota reservation expired"));
                        }
                    }
                } else if matches!(phase, Phase::Confirmed | Phase::Uncertain) {
                    let stored = tx
                        .execution_invocation(project.as_str(), invocation.id.as_str())?
                        .ok_or_else(|| deny("the completed invocation is unavailable"))?;
                    if !matches!(
                        stored.status,
                        InvocationStatus::Ok | InvocationStatus::Failed
                    ) || digest(&stored)? != effect.digest
                    {
                        return Err(deny("the invocation has no matching completed result"));
                    }
                }
                effect.phase = phase;
                tx.put_record(EFFECT, &key, &serde_json::to_value(effect)?)?;
                Ok(())
            })
            .map_err(|error| error.to_string());
        if phase != Phase::Running {
            live.active.remove(invocation.id.as_str());
        }
        if let Err(error) = &result {
            live.error.get_or_insert_with(|| error.clone());
        } else if phase == Phase::Uncertain {
            live.error.get_or_insert_with(|| "An external effect is unconfirmed. Review its recorded result before continuing.".into());
        }
        result
    }

    pub(super) fn verify_pending_approval(
        &self,
        tx: &RecordTransaction<'_>,
        run: &Run,
        request: &ark_runtime::ApprovalRequest,
    ) -> Result<(), OrchError> {
        let scope = request
            .project
            .as_ref()
            .ok_or_else(|| deny("the approval has no execution context"))?;
        let id = request
            .invocation
            .as_ref()
            .ok_or_else(|| deny("the approval has no stored action"))?;
        self.require_scope(tx, run, scope.as_str())?;
        let call = tx
            .execution_invocation(scope.as_str(), id.as_str())?
            .ok_or_else(|| deny("the approval's action is unavailable"))?;
        if call.status != InvocationStatus::PendingApproval
            || call.action != request.action
            || call.input != request.input
            || call.risk != request.risk
        {
            return Err(deny("the approval no longer matches its pending action"));
        }
        let work = if let Some(task) = &request.task {
            task.clone()
        } else {
            let step = run
                .steps
                .iter()
                .find(|step| {
                    step.scope == scope.as_str()
                        && step.receipt.is_none()
                        && step.phase == automation::StepPhase::Running
                })
                .ok_or_else(|| deny("the approval is outside its running step"))?;
            format!("{}/{}", run.id, step.id)
        };
        if call.work.as_deref() != Some(work.as_str()) {
            return Err(deny("the approval belongs to another work item"));
        }
        let descriptor = self.descriptor(run, &request.action)?;
        if call.descriptor.as_deref()
            != Some(
                blake3::hash(&serde_json::to_vec(descriptor)?)
                    .to_hex()
                    .as_str(),
            )
        {
            return Err(deny("the approval's action definition changed"));
        }
        self.allowed(run, &call.action, &call.input, descriptor)?;
        Ok(())
    }

    pub(super) fn verify_child_call(
        &self,
        tx: &RecordTransaction<'_>,
        scope: &str,
        invocation: &str,
        input: &serde_json::Value,
    ) -> Result<Run, OrchError> {
        let run = self.load(tx)?;
        self.require_scope(tx, &run, scope)?;
        let effect: Effect = serde_json::from_value(
            tx.get_record(EFFECT, &identity(&self.run, invocation))?
                .ok_or_else(|| deny("the delegation has no execution reservation"))?,
        )?;
        let call = tx
            .execution_invocation(scope, invocation)?
            .ok_or_else(|| deny("the delegation invocation is unavailable"))?;
        if effect.run != self.run
            || effect.project != scope
            || effect.phase != Phase::Running
            || effect.digest != digest(&call)?
            || call.action != tools::DELEGATE
            || call.status != InvocationStatus::Running
            || &call.input != input
        {
            return Err(deny("the delegation does not match its running invocation"));
        }
        self.allowed(
            &run,
            tools::DELEGATE,
            input,
            self.descriptor(&run, tools::DELEGATE)?,
        )?;
        Ok(run)
    }

    pub(super) fn fail(&self, reason: String) {
        self.live.lock().unwrap().error.get_or_insert(reason);
    }

    pub(super) fn fault(&self) -> Option<String> {
        self.live.lock().unwrap().error.clone()
    }
}

impl CallGuard for RunGuard {
    fn check(
        &self,
        action: &str,
        input: &serde_json::Value,
        descriptor: Option<&ActionDescriptor>,
    ) -> Verdict {
        let mut live = self.live.lock().unwrap();
        if let Some(error) = &live.error {
            return Verdict::Refuse(error.clone());
        }
        let checked =
            self.engine
                .store
                .transact_records::<_, OrchError>(&self.project, |tx| {
                    let run = self.load(tx)?;
                    let Some(descriptor) = descriptor else {
                        if !matches!(
                            action,
                            ark_runtime::catalog::LIST_ACTIONS
                                | ark_runtime::catalog::LOAD_ACTION
                                | ark_runtime::catalog::GET_EVIDENCE
                                | ark_runtime::catalog::SEARCH_EVIDENCE
                        ) {
                            return Err(deny(
                                "this internal operation has no guarded effect contract",
                            ));
                        }
                        if !input.is_object()
                            || matches!(
                                action,
                                ark_runtime::catalog::GET_EVIDENCE
                                    | ark_runtime::catalog::SEARCH_EVIDENCE
                            ) && input[if action == ark_runtime::catalog::GET_EVIDENCE {
                                "invocation"
                            } else {
                                "query"
                            }]
                            .as_str()
                            .is_none_or(|value| value.trim().is_empty())
                        {
                            return Err(deny(
                                "internal reads need their explicit query or evidence reference",
                            ));
                        }
                        if input.get("limit").is_some_and(|value| {
                            value.as_u64().is_none_or(|limit| limit == 0 || limit > 100)
                        }) || ["invocation", "path", "match", "query", "name"].iter().any(
                            |field| {
                                input.get(*field).is_some_and(|value| {
                                    value.as_str().is_none_or(|text| text.len() > 4096)
                                })
                            },
                        ) {
                            return Err(deny(
                                "internal reads need bounded arguments and at most 100 results",
                            ));
                        }
                        if action == ark_runtime::catalog::LOAD_ACTION
                            && !run.revision.draft.authority.actions.iter().any(|allowed| {
                                Some(allowed.action.as_str()) == input["name"].as_str()
                            })
                        {
                            return Err(deny("that action is outside the frozen grant"));
                        }
                        let calls = meta_calls(tx, &self.run)?;
                        let effects = tx
                            .records_matching(EFFECT, "run", &self.run)?
                            .into_iter()
                            .map(serde_json::from_value::<Effect>)
                            .collect::<Result<Vec<_>, _>>()?;
                        let external = effects
                            .iter()
                            .filter(|effect| effect.phase != Phase::Abandoned)
                            .count() as u64;
                        if calls.saturating_add(external)
                            >= u64::from(run.revision.draft.authority.max_calls)
                        {
                            return Err(deny("this execution's call limit was reached"));
                        }
                        let now = now_ms();
                        if !tx.reserve_quota(
                            "automation:all-calls",
                            now / 3_600_000 * 3_600_000,
                            1000,
                        )? {
                            return Err(deny("the automation account limit was reached"));
                        }
                        tx.put_record(
                            META_CALLS,
                            &self.run,
                            &serde_json::to_value(MetaCalls { calls: calls + 1 })?,
                        )?;
                        return Ok(Verdict::Allow);
                    };
                    let allowed = self.allowed(&run, action, input, descriptor)?;
                    Ok(
                        if descriptor.risk == RiskTier::Mutate
                            && (run.activation.is_none()
                                || !allowed.unattended
                                || descriptor.ask_each_time)
                        {
                            Verdict::Ask
                        } else {
                            Verdict::Allow
                        },
                    )
                });
        match checked {
            Ok(verdict) => verdict,
            Err(OrchError::NotAllowed(error)) => Verdict::Refuse(error),
            Err(error) => {
                let error = error.to_string();
                live.error.get_or_insert_with(|| error.clone());
                Verdict::Refuse(error)
            }
        }
    }

    fn prepare(&self, project: &ProjectId, invocation: &Invocation) -> Result<(), String> {
        let mut live = self.live.lock().unwrap();
        if let Some(error) = &live.error {
            return Err(error.clone());
        }
        if live.active.contains(invocation.id.as_str()) {
            return Err("another execution already claimed this action".into());
        }
        let mut claim_lost = false;
        let result=self.engine.store.transact_records::<_,OrchError>(&self.project,|tx| {
            let run=self.load(tx)?;
            self.require_scope(tx, &run, project.as_str())?;
            let pending = tx.execution_invocation(project.as_str(), invocation.id.as_str())?;
            if let Some(pending) = &pending {
                if pending.status != InvocationStatus::PendingApproval || digest(pending)? != digest(invocation)? {
                    claim_lost = digest(pending)? == digest(invocation)? && pending.work == invocation.work && pending.descriptor == invocation.descriptor;
                    return Err(deny("the invocation identity was already used"));
                }
            }
            let key=identity(&self.run,invocation.id.as_str());
            if let Some(value) = tx.get_record(EFFECT, &key)? {
                let effect: Effect = serde_json::from_value(value)?;
                if pending.is_none() || effect.project != project.as_str() || effect.digest != digest(invocation)? || effect.phase != Phase::Prepared || live.active.contains(invocation.id.as_str()) {
                    return Err(deny("this invocation already has an effect reservation"));
                }
                for quota in &effect.quotas {
                    if now_ms() / quota.window_ms * quota.window_ms != quota.start || tx.quota_used(&quota.key, quota.start)?.is_none_or(|used| used == 0) {
                        return Err(deny("the pending action's quota reservation expired"));
                    }
                }
                return Ok(());
            }
            let descriptor=self.descriptor(&run,&invocation.action)?;
            if invocation.risk!=descriptor.risk { return Err(deny("the action risk changed")); }
            let allowed=self.allowed(&run,&invocation.action,&invocation.input,descriptor)?;
            let effects=tx.records_matching(EFFECT,"run",&self.run)?.into_iter().map(serde_json::from_value::<Effect>).collect::<Result<Vec<_>,_>>()?;
            let counted: Vec<_> = effects.iter().filter(|effect|effect.phase!=Phase::Abandoned).collect();
            if effects.len()>=1000 || (counted.len() as u64).saturating_add(meta_calls(tx, &self.run)?)>=u64::from(run.revision.draft.authority.max_calls) || counted.iter().filter(|effect|effect.action==invocation.action).count()>=allowed.per_run as usize {
                return Err(deny("this execution's call limit was reached"));
            }
            if invocation.risk==RiskTier::Mutate {
                if counted.iter().filter(|effect|effect.risk==RiskTier::Mutate).count()>=run.revision.draft.authority.max_mutations as usize { return Err(deny("this execution's change limit was reached")); }
                if effects.iter().any(|effect|effect.risk==RiskTier::Mutate && (effect.phase==Phase::Uncertain || matches!(effect.phase,Phase::Prepared|Phase::Running) && !live.active.contains(&effect.invocation))) {
                    return Err(deny("an earlier change is unconfirmed; review it before making another change"));
                }
            }
            let now=now_ms();
            let subject=serde_json::to_vec(&("v1", &invocation.action, allowed.quota.window_ms))?;
            let quota=format!("automation:{}",blake3::hash(&subject).to_hex());
            let window=now/allowed.quota.window_ms*allowed.quota.window_ms;
            let global_window=now/3_600_000*3_600_000;
            if !tx.reserve_quota(&quota,window,allowed.quota.count)? || !tx.reserve_quota("automation:all-calls",global_window,1000)? { return Err(deny("the automation account limit was reached")); }
            let effect=Effect { run:self.run.clone(),invocation:invocation.id.to_string(),project:project.to_string(),action:invocation.action.clone(),digest:digest(invocation)?,risk:invocation.risk,phase:Phase::Prepared,at_ms:now,
                quotas:vec![QuotaReservation { key:quota,start:window,window_ms:allowed.quota.window_ms },QuotaReservation { key:"automation:all-calls".into(),start:global_window,window_ms:3_600_000 }] };
            tx.insert_record(EFFECT,&key,&serde_json::to_value(effect)?)?;
            Ok(())
        }).map_err(|error|error.to_string());
        if result.is_ok() {
            live.active.insert(invocation.id.to_string());
        } else if let Err(error) = &result {
            if !claim_lost {
                live.error.get_or_insert_with(|| error.clone());
            }
        }
        result
    }
    fn started(&self, project: &ProjectId, invocation: &Invocation) -> Result<(), String> {
        self.update(project, invocation, Phase::Running, true)
    }
    fn ended(&self, project: &ProjectId, invocation: &Invocation) -> Result<(), String> {
        self.update(project, invocation, Self::finished_phase(invocation), false)
    }
    fn aborted(
        &self,
        project: &ProjectId,
        invocation: &Invocation,
        started: bool,
    ) -> Result<(), String> {
        let phase = if !started {
            Phase::Abandoned
        } else if matches!(
            invocation.status,
            InvocationStatus::Ok | InvocationStatus::Failed
        ) {
            Self::finished_phase(invocation)
        } else {
            Phase::Uncertain
        };
        self.update(project, invocation, phase, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_tools::{ActionOutput, Adapter, RunCtx};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counter(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Adapter for Counter {
        fn name(&self) -> &str {
            "count"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![
                ActionDescriptor::new("count.write", "write", RiskTier::Mutate)
                    .auto_eligible()
                    .grant_per("count", &["value"]),
            ]
        }
        async fn run(
            &self,
            _: &str,
            input: serde_json::Value,
            _: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let output = ActionOutput::json(json!({"written": true}));
            Ok(if input["value"] == "unconfirmed" {
                output.with_effect(ark_core::ExternalEffect {
                    id: "external-one".into(),
                    state: ark_core::ExternalEffectState::Unconfirmed,
                    retry_allowed: false,
                    user_message: Some("Verify delivery".into()),
                })
            } else {
                output
            })
        }
    }
    async fn fixture() -> (tempfile::TempDir, Ark, Orchestrator, Run, Arc<AtomicUsize>) {
        fixture_limits(256, 32, true).await
    }
    async fn fixture_limits(
        max_calls: u32,
        max_mutations: u32,
        allow_actions: bool,
    ) -> (tempfile::TempDir, Ark, Orchestrator, Run, Arc<AtomicUsize>) {
        use automation::*;
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let ark = Ark::builder()
            .store_root(dir.path())
            .tool(Arc::new(Counter(calls.clone())))
            .build()
            .unwrap();
        let project = ark.create_project("p", "").unwrap();
        let o = Orchestrator::builder(&ark, &project).build().unwrap();
        let draft = Draft {
            name: "bounded".into(),
            trigger: Trigger::Webhook,
            inputs: vec![],
            steps: vec![automation::Step {
                id: "result".into(),
                name: "Result".into(),
                action: StepAction::Result { inputs: vec![] },
            }],
            authority: Authority {
                max_calls,
                max_mutations,
                actions: if allow_actions {
                    vec![AllowedAction {
                        action: "count.write".into(),
                        inputs: std::collections::BTreeMap::from([("value".into(), Bound::Free)]),
                        unattended: true,
                        per_run: 10,
                        quota: Quota {
                            count: 2,
                            window_ms: 3_600_000,
                        },
                    }]
                } else {
                    vec![]
                },
            },
        };
        let saved = o.save_automation("new", None, None, draft).unwrap();
        let setup = o
            .configure_webhook(
                &saved.automation,
                1,
                "fixture-connection",
                "http://127.0.0.1:1/",
                60,
            )
            .unwrap();
        o.record_webhook_probe(&setup.endpoint, &setup.connection.id, true)
            .unwrap();
        let activation = o
            .prepare_automation_activation(&saved.automation, 1)
            .await
            .unwrap();
        o.activate_automation(&saved.automation, &activation.id, &activation.digest)
            .unwrap();
        let mut run = o
            .admit_webhook_event(
                &setup.endpoint,
                &setup.connection.id,
                "one",
                now_ms().saturating_add(1),
                Default::default(),
            )
            .unwrap()
            .run;
        run.owner = Some(RunOwner {
            token: o.inner.execution_token.clone(),
            pid: std::process::id(),
            pid_start: process::own_start(),
        });
        run.phase = RunPhase::Running;
        run.steps[0].phase = StepPhase::Running;
        ark.engine()
            .store
            .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
                tx.put_record(RUN, &run.id, &serde_json::to_value(&run)?)?;
                Ok(())
            })
            .unwrap();
        (dir, ark, o, run, calls)
    }

    #[tokio::test]
    async fn internal_reads_share_durable_call_limits_and_cannot_write_state() {
        let (_dir, ark, o, run, calls) = fixture_limits(2, 0, false).await;
        let guard = o.inner.run_guard(&run.id, true).unwrap();
        for action in [
            ark_runtime::catalog::RECORD_NOTE,
            ark_runtime::catalog::RECORD_STATE,
        ] {
            assert!(matches!(
                guard.check(action, &json!({"text":"forbidden"}), None),
                Verdict::Refuse(_)
            ));
        }
        assert!(matches!(
            guard.check(
                ark_runtime::catalog::GET_EVIDENCE,
                &json!({"limit":101}),
                None
            ),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            guard.check(
                ark_runtime::catalog::GET_EVIDENCE,
                &json!({"invocation":"own"}),
                None
            ),
            Verdict::Allow
        ));
        drop(guard);
        let delegated = o.inner.run_guard(&run.id, false).unwrap();
        assert!(matches!(
            delegated.check(
                ark_runtime::catalog::SEARCH_EVIDENCE,
                &json!({"query":"own"}),
                None
            ),
            Verdict::Allow
        ));
        drop(delegated);
        let restored = o.inner.run_guard(&run.id, true).unwrap();
        assert!(matches!(
            restored.check(
                ark_runtime::catalog::GET_EVIDENCE,
                &json!({"invocation":"own"}),
                None
            ),
            Verdict::Refuse(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            ark.engine()
                .store
                .transact_records::<_, OrchError>(o.project().id.as_str(), |tx| meta_calls(
                    tx, &run.id
                ))
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn internal_reads_exhaust_adapter_calls_and_survive_store_reopen() {
        let (dir, ark, o, run, calls) = fixture_limits(2, 2, true).await;
        let guard = o.inner.run_guard(&run.id, true).unwrap();
        for _ in 0..2 {
            assert!(matches!(
                guard.check(
                    ark_runtime::catalog::SEARCH_EVIDENCE,
                    &json!({"query":"own"}),
                    None
                ),
                Verdict::Allow
            ));
        }
        let reopened = ark_store::Store::open(dir.path()).unwrap();
        assert_eq!(
            reopened
                .transact_records::<_, OrchError>(o.project().id.as_str(), |tx| meta_calls(
                    tx, &run.id
                ))
                .unwrap(),
            2
        );
        let scope = ProjectId(run.steps[0].scope.clone());
        let work = format!("{}/{}", run.id, run.steps[0].id);
        assert!(ark
            .engine()
            .for_work(&work)
            .guarded(guard)
            .run_action(
                &scope,
                "count.write",
                json!({"value":"outside-budget"}),
                true
            )
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn active_projection_preserves_the_published_revision_after_a_draft_edit() {
        let (_dir, _ark, o, run, _calls) = fixture().await;
        let mut edited = run.revision.draft.clone();
        edited.name = "New manual draft".into();
        edited.trigger = automation::Trigger::Manual;
        o.save_automation("draft-replacement", Some(&run.automation), Some(1), edited)
            .unwrap();
        let entry = o.automation(&run.automation).unwrap();
        assert_eq!(entry.revision.number, 2);
        assert!(matches!(
            entry.revision.draft.trigger,
            automation::Trigger::Manual
        ));
        assert_eq!(entry.active.as_ref().unwrap().number, 1);
        assert!(matches!(
            entry.active.as_ref().unwrap().draft.trigger,
            automation::Trigger::Webhook
        ));
        let page = o.automation_page(None, 10).unwrap();
        assert_eq!(
            page.entries[0].active.as_ref().unwrap().digest,
            run.revision.digest
        );
    }

    #[tokio::test]
    async fn losing_a_pending_claim_does_not_fault_the_winner_or_reset_quotas() {
        struct Together(tokio::sync::Barrier);
        #[async_trait::async_trait]
        impl ark_runtime::Approver for Together {
            async fn decide(
                &self,
                _: &ark_runtime::ApprovalRequest,
            ) -> ark_runtime::ApprovalDecision {
                self.0.wait().await;
                ark_runtime::ApprovalDecision::AllowOnce
            }
        }
        let (_dir, ark, o, run, calls) = fixture().await;
        let scope = ProjectId(run.steps[0].scope.clone());
        let work = format!("{}/{}", run.id, run.steps[0].id);
        let pending = ark
            .engine()
            .for_work(&work)
            .run_action(&scope, "count.write", json!({"value":"first"}), false)
            .await
            .unwrap();
        let guard = o.inner.run_guard(&run.id, true).unwrap();
        let engine = ark.engine().for_work(&work).guarded(guard.clone());
        let gate = ark_runtime::ApprovalGate::new(Arc::new(Together(tokio::sync::Barrier::new(2))));
        let (first, second) = tokio::join!(
            engine.run_pending_with_approval(&scope, &pending.id, &gate),
            engine.run_pending_with_approval(&scope, &pending.id, &gate)
        );
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        guard.ensure_current().unwrap();
        engine
            .run_action(&scope, "count.write", json!({"value":"second"}), true)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(engine
            .run_action(&scope, "count.write", json!({"value":"third"}), true)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn action_quota_cannot_be_reset_by_arguments_or_descendant_scope() {
        let (_dir, ark, o, run, calls) = fixture().await;
        let guard = o.inner.run_guard(&run.id, true).unwrap();
        let engine = ark.engine().guarded(guard.clone());
        let scope = ProjectId(run.steps[0].scope.clone());
        engine
            .run_action(&scope, "count.write", json!({"value": "first"}), true)
            .await
            .unwrap();
        let child = Project::new("second child", "").with_parent(o.project().id.clone());
        ark.engine().store.put_project(&child).unwrap();
        let mut task = TaskRecord::queued(
            "quota-child".into(),
            run.project.clone(),
            "child".into(),
            &TaskSpec::new("bounded descendant"),
            Origin::External,
            Default::default(),
            vec![],
        );
        task.run = Some(run.id.clone());
        task.execution_step = Some(run.steps[0].id.clone());
        task.execution_scope = Some(child.id.to_string());
        task.execution_owner = Some(o.inner.execution_token.clone());
        ark.engine()
            .store
            .put_record(
                TASK,
                &task.id,
                &run.project,
                &serde_json::to_value(&task).unwrap(),
            )
            .unwrap();
        engine
            .run_action(&child.id, "count.write", json!({"value": "second"}), true)
            .await
            .unwrap();
        assert!(engine
            .run_action(&scope, "count.write", json!({"value": "third"}), true)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let effects = ark
            .engine()
            .store
            .list_records(EFFECT, Some(o.project().id.as_str()))
            .unwrap();
        assert_eq!(effects.len(), 2);
        assert!(effects.iter().all(|effect| effect["phase"] == "confirmed"));
        let foreign = ark.create_project("other root", "").unwrap();
        assert!(engine
            .run_action(
                &foreign.id,
                "count.write",
                json!({"value": "foreign"}),
                true
            )
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn revocation_between_check_and_prepare_blocks_effect_and_missing_reservation_is_error() {
        let (_dir, ark, o, run, calls) = fixture().await;
        let guard = o.inner.run_guard(&run.id, true).unwrap();
        let descriptor = Counter(calls.clone()).actions().remove(0);
        assert!(matches!(
            guard.check("count.write", &json!({"value": 1}), Some(&descriptor)),
            Verdict::Allow
        ));
        let activation: automations::PreparedActivation = serde_json::from_value(
            ark.engine()
                .store
                .get_record(automations::ACTIVATION, run.activation.as_deref().unwrap())
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        o.revoke_automation_activation(&activation.value.id, &activation.value.digest)
            .unwrap();
        let invocation = Invocation::new("count.write", RiskTier::Mutate, json!({"value": 1}));
        let scope = ProjectId(run.steps[0].scope.clone());
        assert!(guard.prepare(&scope, &invocation).is_err());
        assert!(guard.ended(&scope, &invocation).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(guard.fault().is_some());
    }

    #[tokio::test]
    async fn successful_invocation_with_unconfirmed_effect_stops_descendants() {
        let (_dir, ark, o, run, calls) = fixture().await;
        let guard = o.inner.run_guard(&run.id, true).unwrap();
        let engine = ark.engine().guarded(guard.clone());
        let scope = ProjectId(run.steps[0].scope.clone());
        let result = engine
            .run_action(&scope, "count.write", json!({"value":"unconfirmed"}), true)
            .await
            .unwrap();
        assert_eq!(
            result.effect.unwrap().state,
            ark_core::ExternalEffectState::Unconfirmed
        );
        assert!(guard.fault().unwrap().contains("unconfirmed"));
        assert!(engine
            .run_action(&scope, "count.write", json!({"value":"next"}), true)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let effects = ark
            .engine()
            .store
            .list_records(EFFECT, Some(o.project().id.as_str()))
            .unwrap();
        assert_eq!(effects[0]["phase"], "uncertain");
    }

    #[tokio::test]
    async fn a_delayed_call_cannot_start_after_its_step_ends() {
        let (_dir, ark, o, mut run, calls) = fixture().await;
        let guard = o.inner.run_guard(&run.id, true).unwrap();
        let scope = ProjectId(run.steps[0].scope.clone());
        let mut invocation =
            Invocation::new("count.write", RiskTier::Mutate, json!({"value":"delayed"}));
        guard.prepare(&scope, &invocation).unwrap();
        invocation.status = InvocationStatus::Running;
        ark.engine()
            .store
            .put_invocation(&scope, &invocation)
            .unwrap();
        run.steps[0].phase = automation::StepPhase::Completed;
        ark.engine()
            .store
            .transact_records::<_, OrchError>(o.project().id.as_str(), |tx| {
                tx.put_record(automation::RUN, &run.id, &serde_json::to_value(&run)?)
                    .map_err(Into::into)
            })
            .unwrap();
        assert!(guard.started(&scope, &invocation).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn exact_null_bound_requires_present_field() {
        let (_dir, ark, o, mut run, calls) = fixture().await;
        run.revision.draft.authority.actions[0].inputs.insert(
            "value".into(),
            Bound::Literal {
                value: serde_json::Value::Null,
            },
        );
        run.revision.digest = run.revision.draft.digest().unwrap();
        run.activation = None;
        ark.engine()
            .store
            .transact_records::<_, OrchError>(o.project().id.as_str(), |tx| {
                tx.put_record(automation::RUN, &run.id, &serde_json::to_value(&run)?)?;
                Ok(())
            })
            .unwrap();
        let guard = o.inner.run_guard(&run.id, true).unwrap();
        let descriptor = Counter(calls).actions().remove(0);
        assert!(matches!(
            guard.check("count.write", &json!({}), Some(&descriptor)),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            guard.check("count.write", &json!({"value": null}), Some(&descriptor)),
            Verdict::Ask
        ));
    }

    #[tokio::test]
    async fn quota_storage_failure_prevents_adapter_effect() {
        let (dir, ark, o, run, calls) = fixture().await;
        let guard = o.inner.run_guard(&run.id, true).unwrap();
        let connection = rusqlite::Connection::open(dir.path().join("meta.db")).unwrap();
        connection.execute("DROP TABLE quota_windows", []).unwrap();
        let result = ark
            .engine()
            .guarded(guard.clone())
            .run_action(
                &ProjectId(run.steps[0].scope.clone()),
                "count.write",
                json!({"value":"not performed"}),
                true,
            )
            .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(guard.fault().is_some());
        assert!(ark
            .engine()
            .store
            .list_records(EFFECT, Some(o.project().id.as_str()))
            .unwrap()
            .is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn independent_runs_share_the_same_store_wide_action_quota() {
        let (_dir, ark, o, first, calls) = fixture().await;
        let other = Orchestrator::builder(&ark, o.project()).build().unwrap();
        let automation::RunOrigin::Webhook { endpoint, .. } = &first.origin else {
            panic!("fixture needs a webhook execution");
        };
        let connection = other.webhook_endpoint(endpoint).unwrap().active.unwrap().id;
        let mut second = other
            .admit_webhook_event(
                endpoint,
                &connection,
                "second",
                now_ms().saturating_add(1),
                Default::default(),
            )
            .unwrap()
            .run;
        second.phase = automation::RunPhase::Running;
        second.owner = Some(automation::RunOwner {
            token: other.inner.execution_token.clone(),
            pid: std::process::id(),
            pid_start: process::own_start(),
        });
        ark.engine()
            .store
            .transact_records::<_, OrchError>(o.project().id.as_str(), |tx| {
                tx.put_record(automation::RUN, &second.id, &serde_json::to_value(&second)?)?;
                Ok(())
            })
            .unwrap();
        let one = ark
            .engine()
            .guarded(o.inner.run_guard(&first.id, true).unwrap());
        let two = ark
            .engine()
            .guarded(other.inner.run_guard(&second.id, true).unwrap());
        let first_scope = ProjectId(first.steps[0].scope.clone());
        let second_scope = ProjectId(second.steps[0].scope.clone());
        let (a, b, c) = tokio::join!(
            one.run_action(&first_scope, "count.write", json!({"value":"one"}), true),
            two.run_action(&second_scope, "count.write", json!({"value":"two"}), true),
            one.run_action(&first_scope, "count.write", json!({"value":"three"}), true)
        );
        assert_eq!(
            [a.is_ok(), b.is_ok(), c.is_ok()]
                .into_iter()
                .filter(|ok| *ok)
                .count(),
            2
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
