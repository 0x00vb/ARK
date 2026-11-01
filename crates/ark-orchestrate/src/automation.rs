//! Reusable sequential work. Revisions describe execution; layout is separate data.
use crate::When;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

pub(crate) const DEFINITION: &str = "automation_definition";
pub(crate) const REVISION: &str = "automation_revision";
pub(crate) const RUN: &str = "automation_run";
pub(crate) const START_RECEIPT: &str = "automation_start_receipt";
pub(crate) const MANUAL_START: &str = "automation_manual_start";
pub(crate) const LAYOUT: &str = "automation_layout";
pub const MAX_STEPS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    Manual,
    Schedule { when: When },
    Webhook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Port {
    Text,
    Files,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Binding {
    Literal { text: String },
    Artifact { id: String },
    Input { name: String, port: Port },
    Step { step: String, port: Port },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub name: String,
    pub port: Port,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Bound {
    Literal { value: Value },
    Input { name: String },
    WorkspacePath { prefix: String },
    Free,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quota {
    pub count: u32,
    pub window_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedAction {
    pub action: String,
    pub inputs: BTreeMap<String, Bound>,
    #[serde(default)]
    pub unattended: bool,
    pub per_run: u32,
    pub quota: Quota,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    #[serde(default)]
    pub actions: Vec<AllowedAction>,
    #[serde(default = "default_calls")]
    pub max_calls: u32,
    #[serde(default = "default_mutations")]
    pub max_mutations: u32,
}
fn default_calls() -> u32 {
    256
}
fn default_mutations() -> u32 {
    32
}
impl Default for Authority {
    fn default() -> Self {
        Self {
            actions: vec![],
            max_calls: default_calls(),
            max_mutations: default_mutations(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepAction {
    Agent {
        target: crate::WorkTarget,
        prompt: String,
        #[serde(default)]
        inputs: Vec<Binding>,
    },
    Approval {
        title: String,
        #[serde(default)]
        inputs: Vec<Binding>,
    },
    Result {
        inputs: Vec<Binding>,
    },
}

impl StepAction {
    pub fn inputs(&self) -> &[Binding] {
        match self {
            Self::Agent { inputs, .. }
            | Self::Approval { inputs, .. }
            | Self::Result { inputs } => inputs,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub name: String,
    pub action: StepAction,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub name: String,
    pub trigger: Trigger,
    #[serde(default)]
    pub inputs: Vec<Input>,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub authority: Authority,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Definition {
    pub id: String,
    pub project: String,
    pub revision: u64,
    pub enabled: bool,
    #[serde(default)]
    pub active_revision: Option<u64>,
    pub schedule: Option<String>,
    #[serde(default)]
    pub webhook: Option<String>,
    pub activation: Option<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LegacyPolicies {
    pub schedule: String,
    pub source_version: u64,
    pub source_digest: String,
    pub step: String,
    pub after: Vec<String>,
    pub budget: Option<crate::Budget>,
    pub rules: Vec<ark_core::RuleBinding>,
    pub work: Option<String>,
    pub report_to: Option<String>,
    pub max_attempts: u32,
    pub retry_delay_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyAdoption {
    pub schedule: String,
    pub expected: u64,
    #[serde(default)]
    pub config: Option<String>,
}

#[derive(Serialize)]
pub struct LegacyProjection {
    pub policies: LegacyPolicies,
    pub schedule: String,
    pub version: u64,
    pub draft: Draft,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Revision {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy: Option<LegacyPolicies>,
    pub automation: String,
    pub number: u64,
    pub digest: String,
    pub draft: Draft,
    pub created_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Layout {
    pub version: u64,
    pub positions: BTreeMap<String, Position>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

fn bounded(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(|c| c.is_control())
}
fn identifier(value: &str) -> bool {
    bounded(value, 64)
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub step: Option<String>,
    pub field: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct InputPreview {
    pub revision: u64,
    pub inputs: BTreeMap<String, InputValue>,
    pub bindings: Vec<InputPreviewBinding>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InputPreviewBinding {
    pub step: String,
    pub input: String,
    pub port: Port,
}

impl Draft {
    pub fn activation_issues(&self) -> Vec<ValidationIssue> {
        if matches!(self.trigger, Trigger::Schedule { .. })
            && self.inputs.iter().any(|input| input.required)
        {
            vec![ValidationIssue { step: None, field: "Execution inputs".into(), message: "Schedules cannot supply required inputs. Use explicit step values or make these inputs optional.".into() }]
        } else {
            vec![]
        }
    }

    pub fn storage_validated(self) -> Result<Self, String> {
        if self.steps.len() > MAX_STEPS
            || self.inputs.len() > 32
            || self.authority.actions.len() > 128
        {
            return Err("the draft exceeds its step, input or permission limit".into());
        }
        let mut ids = HashSet::new();
        if self.steps.iter().any(|step| {
            !identifier(&step.id) || !ids.insert(&step.id) || step.action.inputs().len() > 32
        }) {
            return Err(
                "draft steps need unique bounded identifiers and at most 32 bindings".into(),
            );
        }
        if serde_json::to_vec(&self)
            .map_err(|error| error.to_string())?
            .len()
            > 256 * 1024
        {
            return Err("the automation definition is too large".into());
        }
        if self.validation_issues().is_empty() {
            self.validated()
        } else {
            Ok(self)
        }
    }

    pub fn validation_issues(&self) -> Vec<ValidationIssue> {
        let Err(message) = self.clone().validated() else {
            return vec![];
        };
        let mut seen = HashSet::new();
        let step = self.steps.iter().enumerate().find(|(index, step)| {
            let duplicate = !seen.insert(&step.id);
            if message.starts_with("step identifiers") { return !identifier(&step.id) || !bounded(&step.name, 240) || duplicate; }
            if message.starts_with("an agent step") { return matches!(&step.action, StepAction::Agent { target, prompt, .. } if prompt.trim().is_empty() || prompt.len() > 16_000 || matches!(target, crate::WorkTarget::Agent { name } if !bounded(name, 64)) || matches!(target, crate::WorkTarget::General { model: Some(model) } if !bounded(model, 1000))); }
            if message.starts_with("an approval") { return matches!(&step.action, StepAction::Approval { title, .. } if !bounded(title, 500)); }
            if message.starts_with("a step supports") { return step.action.inputs().len() > 32; }
            step.action.inputs().iter().any(|binding| match binding {
                Binding::Literal { text } => message.starts_with("a literal") && text.len() > 16_000,
                Binding::Artifact { id } => message.starts_with("an artifact") && !bounded(id, 128),
                Binding::Input { name, port } => message.starts_with("an input binding") && !self.inputs.iter().any(|input| input.name == *name && input.port == *port),
                Binding::Step { step, .. } => message.starts_with("a binding must") && (*index == 0 || self.steps[*index - 1].id != *step),
            })
        }).map(|(_, step)| step.id.clone());
        let field = if message.starts_with("an agent step") {
            "Task"
        } else if message.starts_with("an approval") {
            "Question"
        } else if message.starts_with("step identifiers")
            || message.starts_with("an automation needs a name")
        {
            "Name"
        } else if message.contains("one to eight") {
            "Steps"
        } else if message.starts_with("an authority") || message.starts_with("action") {
            "Permissions"
        } else if step.is_some() || message.contains("input") || message.contains("binding") {
            "Inputs"
        } else if matches!(&self.trigger, Trigger::Schedule { when } if when.clone().normalized().is_err())
        {
            "Start"
        } else {
            "Permissions"
        };
        vec![ValidationIssue {
            step,
            field: field.into(),
            message,
        }]
    }

    pub fn validated(mut self) -> Result<Self, String> {
        if !bounded(&self.name, 240) {
            return Err("an automation needs a name of at most 240 bytes".into());
        }
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS {
            return Err("an automation needs one to eight steps".into());
        }
        if let Trigger::Schedule { when } = &mut self.trigger {
            *when = when.normalized()?;
        }
        if self.inputs.len() > 32 {
            return Err("an automation supports at most 32 inputs".into());
        }
        let mut inputs = BTreeMap::new();
        for input in &self.inputs {
            if !identifier(&input.name) || inputs.insert(&input.name, input.port).is_some() {
                return Err("input names must be unique identifiers".into());
            }
        }
        let mut previous = BTreeMap::new();
        let mut previous_step = None;
        for step in &self.steps {
            if !identifier(&step.id) || !bounded(&step.name, 240) || previous.contains_key(&step.id)
            {
                return Err("step identifiers and names must be bounded and unique".into());
            }
            match &step.action {
                StepAction::Agent { target, prompt, .. }
                    if matches!(target, crate::WorkTarget::Agent { name } if !bounded(name,64))
                        || matches!(target, crate::WorkTarget::General { model: Some(model) } if !bounded(model,1000))
                        || prompt.trim().is_empty()
                        || prompt.len() > 16_000 =>
                {
                    return Err("an agent step needs an existing agent and a bounded prompt".into())
                }
                StepAction::Approval { title, .. } if !bounded(title, 500) => {
                    return Err("an approval needs a bounded title".into())
                }
                _ => {}
            }
            if step.action.inputs().len() > 32 {
                return Err("a step supports at most 32 bindings".into());
            }
            for binding in step.action.inputs() {
                match binding {
                    Binding::Literal { text } if text.len() > 16_000 => {
                        return Err("a literal input is too long".into())
                    }
                    Binding::Artifact { id } if !bounded(id, 128) => {
                        return Err("an artifact reference is invalid".into())
                    }
                    Binding::Input { name, port } if inputs.get(name) != Some(port) => {
                        return Err("an input binding must match a declared port".into())
                    }
                    Binding::Step { step, .. }
                        if previous.get(step) != Some(&true)
                            || previous_step != Some(step.as_str()) =>
                    {
                        return Err("a binding must name the immediately preceding step".into())
                    }
                    _ => {}
                }
            }
            previous.insert(&step.id, true);
            previous_step = Some(step.id.as_str());
        }
        if self.authority.max_calls == 0
            || self.authority.max_calls > 1000
            || self.authority.max_mutations > self.authority.max_calls
        {
            return Err(
                "run limits must allow one to 1000 calls with no larger mutation limit".into(),
            );
        }
        let mut actions = HashSet::new();
        if self.authority.actions.len() > 128 {
            return Err("an automation supports at most 128 allowed actions".into());
        }
        for action in &self.authority.actions {
            if !bounded(&action.action, 128)
                || !actions.insert(&action.action)
                || action.per_run == 0
                || action.per_run > 1000
                || action.quota.count == 0
                || action.quota.window_ms < 1000
                || action.quota.window_ms > 30 * 86_400_000
            {
                return Err("action limits must be explicit, positive and bounded".into());
            }
            if action.inputs.len() > 64 {
                return Err("an action has too many input bounds".into());
            }
            for bound in action.inputs.values() {
                match bound {
                    Bound::Input { name } if !inputs.contains_key(name) => {
                        return Err("an authority binding names an unknown input".into())
                    }
                    Bound::WorkspacePath { prefix }
                        if prefix.is_empty()
                            || std::path::Path::new(prefix).is_absolute()
                            || std::path::Path::new(prefix).components().any(|part| {
                                !matches!(
                                    part,
                                    std::path::Component::Normal(_) | std::path::Component::CurDir
                                )
                            }) =>
                    {
                        return Err("a path bound must stay within the workspace".into())
                    }
                    Bound::Literal { value }
                        if serde_json::to_vec(value)
                            .map_err(|error| error.to_string())?
                            .len()
                            > 16_000 =>
                    {
                        return Err("an authority value is too long".into())
                    }
                    _ => {}
                }
            }
        }
        self.authority
            .actions
            .sort_by(|a, b| a.action.cmp(&b.action));
        if serde_json::to_vec(&self)
            .map_err(|error| error.to_string())?
            .len()
            > 256 * 1024
        {
            return Err("the automation definition is too large".into());
        }
        Ok(self)
    }

    pub fn digest(&self) -> Result<String, serde_json::Error> {
        Ok(format!(
            "v1:{}",
            blake3::hash(&serde_json::to_vec(self)?).to_hex()
        ))
    }
}

impl Layout {
    pub fn validate(&self, revision: &Revision) -> Result<(), String> {
        if self.positions.len() > MAX_STEPS
            || self.positions.iter().any(|(id, position)| {
                !revision.draft.steps.iter().any(|step| &step.id == id)
                    || !position.x.is_finite()
                    || !position.y.is_finite()
                    || position.x.abs() > 100_000.0
                    || position.y.abs() > 100_000.0
            })
        {
            return Err(
                "canvas positions must name existing steps and stay within the canvas".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunOrigin {
    Manual {
        request: String,
    },
    Schedule {
        schedule: String,
        slot_ms: u64,
    },
    ScheduleManual {
        schedule: String,
        request: String,
    },
    Webhook {
        endpoint: String,
        source: String,
        account: String,
        event: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputValue {
    Text { value: String },
    Files { ids: Vec<String> },
}

impl InputValue {
    pub fn port(&self) -> Port {
        match self {
            Self::Text { .. } => Port::Text,
            Self::Files { .. } => Port::Files,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Pending,
    Running,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}
impl RunPhase {
    pub fn is_final(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepPhase {
    Pending,
    Running,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Output {
    pub text: String,
    pub files: Vec<crate::ArtifactRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunQuestion {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub digest: String,
    pub decision: Option<ark_runtime::ApprovalDecision>,
    #[serde(default)]
    pub action: Option<ark_runtime::ApprovalRequest>,
    #[serde(default)]
    pub files: Vec<crate::ArtifactRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepExecution {
    pub id: String,
    pub scope: String,
    pub conversation: String,
    pub phase: StepPhase,
    pub receipt: Option<String>,
    pub output: Option<Output>,
    pub question: Option<RunQuestion>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RunOwner {
    pub token: String,
    pub pid: u32,
    pub pid_start: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub project: String,
    pub automation: String,
    pub revision: Revision,
    pub activation: Option<String>,
    pub selections: BTreeMap<String, FrozenSelection>,
    pub agents: BTreeMap<String, crate::AgentSpec>,
    pub descriptors: BTreeMap<String, ark_core::ActionDescriptor>,
    pub origin: RunOrigin,
    pub inputs: BTreeMap<String, InputValue>,
    pub input_digest: String,
    pub phase: RunPhase,
    pub work: String,
    pub conversation: String,
    pub steps: Vec<StepExecution>,
    #[serde(default)]
    pub questions: BTreeMap<String, RunQuestion>,
    pub output: Output,
    pub error: Option<String>,
    pub cancel_requested: bool,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub(crate) owner: Option<RunOwner>,
}

impl RunOrigin {
    pub fn validate(&self) -> Result<(), String> {
        let values: Vec<&str> = match self {
            Self::Manual { request } => vec![request],
            Self::Schedule { schedule, .. } => vec![schedule],
            Self::ScheduleManual { schedule, request } => vec![schedule, request],
            Self::Webhook {
                endpoint,
                source,
                account,
                event,
            } => vec![endpoint, source, account, event],
        };
        if values.into_iter().any(|value| !bounded(value, 256)) {
            return Err("an execution needs a bounded trigger identity".into());
        }
        Ok(())
    }
    pub(crate) fn input_digest(
        &self,
        automation: &str,
        revision: u64,
        inputs: &BTreeMap<String, InputValue>,
    ) -> Result<String, serde_json::Error> {
        Ok(format!(
            "v1:{}",
            blake3::hash(&serde_json::to_vec(&(
                automation,
                matches!(self, Self::Manual { .. } | Self::ScheduleManual { .. })
                    .then_some(revision),
                self,
                inputs
            ))?)
            .to_hex()
        ))
    }
    pub fn run_id(&self, project: &str, automation: &str) -> Result<String, serde_json::Error> {
        Ok(format!(
            "ar-{}",
            blake3::hash(&serde_json::to_vec(&(project, automation, self))?).to_hex()
        ))
    }
}

impl Revision {
    pub fn computed_digest(&self) -> Result<String, serde_json::Error> {
        if self.legacy.is_none() {
            return self.draft.digest();
        }
        Ok(format!(
            "v2:{}",
            blake3::hash(&serde_json::to_vec(&(&self.draft, &self.legacy))?).to_hex()
        ))
    }

    pub fn validate_inputs(&self, inputs: &BTreeMap<String, InputValue>) -> Result<(), String> {
        if inputs.len() > 32 {
            return Err("too many automation inputs".into());
        }
        for input in &self.draft.inputs {
            match inputs.get(&input.name) {
                None if input.required => return Err(format!("input {} is required", input.name)),
                Some(value) if value.port() != input.port => {
                    return Err(format!("input {} has another type", input.name))
                }
                _ => {}
            }
        }
        for (name, value) in inputs {
            if !self.draft.inputs.iter().any(|input| &input.name == name) {
                return Err(format!("input {name} is not declared"));
            }
            match value {
                InputValue::Text { value } if value.len() > 64_000 => {
                    return Err("a text input is too long".into())
                }
                InputValue::Files { ids }
                    if ids.len() > 32 || ids.iter().any(|id| !bounded(id, 128)) =>
                {
                    return Err("file inputs need bounded artifact references".into())
                }
                _ => {}
            }
        }
        if serde_json::to_vec(inputs)
            .map_err(|error| error.to_string())?
            .len()
            > 256 * 1024
        {
            return Err("the automation inputs are too large".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SaveReceipt {
    pub automation: String,
    pub revision: u64,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManualStart {
    #[serde(default)]
    pub previous: Option<String>,
    pub automation: String,
    pub expected: u64,
    pub request: String,
    pub inputs: BTreeMap<String, InputValue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartReceipt {
    pub request: ManualStart,
    pub run: String,
    pub work: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub active: Option<Revision>,
    pub last_start: Option<StartReceipt>,
    pub pending_start: Option<ManualStart>,
    pub webhook_availability: Option<WebhookAvailability>,
    pub definition: Definition,
    pub revision: Revision,
}

#[derive(Serialize)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub next: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrozenSelection {
    pub model: String,
    pub agent: Option<crate::AgentSpec>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationPhase {
    Prepared,
    Active,
    Revoked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Activation {
    #[serde(default)]
    pub webhook: Option<WebhookSnapshot>,
    pub id: String,
    pub automation: String,
    pub revision: u64,
    pub digest: String,
    pub selections: BTreeMap<String, FrozenSelection>,
    pub agents: BTreeMap<String, crate::AgentSpec>,
    pub descriptors: BTreeMap<String, ark_core::ActionDescriptor>,
    pub phase: ActivationPhase,
    pub created_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Occurrence {
    pub at_ms: u64,
    pub label: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Preview {
    pub digest: String,
    pub trigger: Trigger,
    pub next: Vec<Occurrence>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunSummary {
    pub id: String,
    pub work: String,
    pub conversation: String,
    pub revision: u64,
    pub origin: RunOrigin,
    pub phase: RunPhase,
    pub error: Option<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
}

#[derive(Serialize)]
pub struct RunPage {
    pub runs: Vec<RunSummary>,
    pub next: Option<String>,
}

pub(crate) const WEBHOOK: &str = "automation_webhook";
pub(crate) const WEBHOOK_SETUP: &str = "automation_webhook_setup";
pub(crate) const WEBHOOK_LOG: &str = "automation_webhook_log";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookAvailability {
    Pending,
    Local,
    Verified,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookConnection {
    pub id: String,
    pub url: String,
    pub max_per_hour: u32,
    pub availability: WebhookAvailability,
    pub verified_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookSnapshot {
    pub endpoint: String,
    pub connection: String,
    pub account: String,
    pub url: String,
    pub max_per_hour: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookEndpoint {
    pub id: String,
    pub project: String,
    pub automation: String,
    pub account: String,
    pub draft: WebhookConnection,
    pub active: Option<WebhookConnection>,
    #[serde(default)]
    pub active_since_ms: u64,
    pub enabled: bool,
    pub created_ms: u64,
    pub updated_ms: u64,
}
impl WebhookEndpoint {
    pub fn draft_snapshot(&self) -> WebhookSnapshot {
        WebhookSnapshot {
            endpoint: self.id.clone(),
            connection: self.draft.id.clone(),
            account: self.account.clone(),
            url: self.draft.url.clone(),
            max_per_hour: self.draft.max_per_hour,
        }
    }
    pub fn active_snapshot(&self) -> Option<WebhookSnapshot> {
        self.active.as_ref().map(|active| WebhookSnapshot {
            endpoint: self.id.clone(),
            connection: active.id.clone(),
            account: self.account.clone(),
            url: active.url.clone(),
            max_per_hour: active.max_per_hour,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookSetupReceipt {
    pub endpoint: String,
    pub request: String,
    pub digest: String,
    pub connection: WebhookConnection,
    pub created_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Preview,
    Received,
    Rejected,
    Admitted,
    Duplicate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookDelivery {
    pub at_ms: u64,
    pub state: DeliveryState,
    pub reason: Option<String>,
    pub run: Option<String>,
}

impl Activation {
    pub(crate) fn computed_digest(&self, revision: &str) -> Result<String, serde_json::Error> {
        Ok(format!(
            "v2:{}",
            blake3::hash(&serde_json::to_vec(&(
                revision,
                &self.selections,
                &self.agents,
                &self.descriptors,
                &self.webhook
            ))?)
            .to_hex()
        ))
    }
}

pub struct WebhookAdmission {
    pub run: Run,
    pub duplicate: bool,
}
