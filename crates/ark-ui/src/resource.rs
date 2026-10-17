//! Stable identities for resources a chat or project opens (ADR 0052, 0060).
//!
//! A resource reference names its owning runtime, project replica and opening chat. The host
//! validates all three before doing I/O. Keeping this type in the protocol prevents a pane from
//! silently rebinding to whichever chat the window happens to show later.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// The runtime, project replica and chat that opened a resource.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceLocation {
    pub runtime: String,
    pub project: String,
    pub session: String,
}

/// What granted access to a resource. Old references omit this field and remain chat-scoped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum ResourceAuthority {
    #[default]
    Chat,
    /// An opaque host-issued route. Project and runtime strings alone grant nothing.
    Project { route: String },
}

/// The authority and lifetime rules an id follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Terminal,
    File,
    Process,
    Artifact,
    Invocation,
    Browser,
    Picture,
    Extension,
    Project,
}

/// A resource in one runtime. `id` is opaque except for file paths.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceRef {
    pub location: ResourceLocation,
    pub kind: ResourceKind,
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "ResourceAuthority::is_chat")]
    pub authority: ResourceAuthority,
}

impl ResourceAuthority {
    pub fn is_chat(&self) -> bool {
        matches!(self, Self::Chat)
    }
}

/// Every operation that may cross a runtime boundary.
///
/// The input remains JSON because file editors and consumer extensions have their own typed
/// payloads. The operation name and the resource kind are closed enums, so adding a new route is
/// an explicit Rust change and an unrecognised operation fails before I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceOperationKind {
    Subscribe,
    Unsubscribe,
    TerminalOpen,
    TerminalRead,
    TerminalWrite,
    TerminalResize,
    TerminalClose,
    Processes,
    ProcessOutput,
    ProcessStop,
    Files,
    FileRead,
    FileWrite,
    FileEdit,
    FileMedia,
    FileSnapshotOpen,
    FileSnapshotStatus,
    FileSnapshotCancel,
    FileSnapshotChunk,
    FileSnapshotClose,
    FileProbe,
    FileVersion,
    FileRestore,
    UploadParts,
    UploadAdd,
    UploadCancel,
    FileOpen,
    Workspace,
    State,
    Graph,
    TaskTrace,
    Task,
    TaskCall,
    OrchEvents,
    Agents,
    SpawnAgent,
    SubmitTask,
    CancelTask,
    RetryTask,
    Schedules,
    Schedule,
    Unschedule,
    SchedulePause,
    ScheduleResume,
    ScheduleRun,
    Automations,
    Automation,
    AutomationPause,
    AutomationResume,
    AutomationStop,
    AutomationDiscard,
    Board,
    BoardAdd,
    BoardChange,
    BoardRemove,
    BoardGive,
    SuspendAgent,
    ResumeAgent,
    TerminateAgent,
    MessageAgent,
    Profiles,
    Grants,
    GrantRemove,
    Skills,
    Tools,
    Usage,
    Artifacts,
    Artifact,
    Invocation,
    Picture,
    ComputerPicture,
    RunAction,
    Focus,
    Extension,
    BrowserOpen,
    BrowserSnapshot,
    BrowserNavigate,
    BrowserResize,
    BrowserPointer,
    BrowserKey,
    BrowserText,
    BrowserWheel,
    BrowserBack,
    BrowserReload,
    BrowserStop,
    BrowserClose,
}

impl ResourceOperationKind {
    /// Whether this operation is valid for the reference's authority class.
    pub fn accepts(self, kind: ResourceKind) -> bool {
        use ResourceKind as R;
        use ResourceOperationKind as O;
        match self {
            O::Subscribe | O::Unsubscribe => matches!(kind, R::Terminal | R::Process | R::File),
            O::TerminalOpen
            | O::TerminalRead
            | O::TerminalWrite
            | O::TerminalResize
            | O::TerminalClose => kind == R::Terminal,
            O::Processes | O::ProcessOutput | O::ProcessStop => kind == R::Process,
            O::Files
            | O::FileRead
            | O::FileWrite
            | O::FileEdit
            | O::FileMedia
            | O::FileSnapshotOpen
            | O::FileSnapshotStatus
            | O::FileSnapshotCancel
            | O::FileSnapshotChunk
            | O::FileSnapshotClose
            | O::FileProbe
            | O::FileVersion
            | O::FileRestore
            | O::UploadParts
            | O::UploadAdd
            | O::UploadCancel
            | O::FileOpen => kind == R::File,
            O::Artifacts | O::Artifact => kind == R::Artifact,
            O::Invocation => kind == R::Invocation,
            O::Picture | O::ComputerPicture => kind == R::Picture,
            O::Extension => kind == R::Extension,
            O::BrowserOpen
            | O::BrowserSnapshot
            | O::BrowserNavigate
            | O::BrowserResize
            | O::BrowserPointer
            | O::BrowserKey
            | O::BrowserText
            | O::BrowserWheel
            | O::BrowserBack
            | O::BrowserReload
            | O::BrowserStop
            | O::BrowserClose => kind == R::Browser,
            O::Workspace
            | O::State
            | O::Graph
            | O::TaskTrace
            | O::Task
            | O::TaskCall
            | O::OrchEvents
            | O::Agents
            | O::SpawnAgent
            | O::SubmitTask
            | O::CancelTask
            | O::RetryTask
            | O::Schedules
            | O::Schedule
            | O::Unschedule
            | O::SchedulePause
            | O::ScheduleResume
            | O::ScheduleRun
            | O::Automations
            | O::Automation
            | O::AutomationPause
            | O::AutomationResume
            | O::AutomationStop
            | O::AutomationDiscard
            | O::Board
            | O::BoardAdd
            | O::BoardChange
            | O::BoardRemove
            | O::BoardGive
            | O::SuspendAgent
            | O::ResumeAgent
            | O::TerminateAgent
            | O::MessageAgent
            | O::Profiles
            | O::Grants
            | O::GrantRemove
            | O::Skills
            | O::Tools
            | O::Usage
            | O::RunAction
            | O::Focus => kind == R::Project,
        }
    }
}

/// One typed resource operation from the page or another runtime.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceOperation {
    pub kind: ResourceOperationKind,
    #[serde(default)]
    pub input: Value,
}

/// Active pane aliases for intrinsic project resources. It is deliberately ephemeral: current
/// state or cursor replay rebuilds it after reconnect.
#[derive(Default)]
pub(crate) struct ResourceSubscribers {
    refs: Mutex<HashMap<(ResourceKind, String, String), HashSet<ResourceRef>>>,
}

impl ResourceSubscribers {
    pub fn subscribe(&self, kind: ResourceKind, project: &str, id: &str, r: ResourceRef) {
        self.refs
            .lock()
            .unwrap()
            .entry((kind, project.to_string(), id.to_string()))
            .or_default()
            .insert(r);
    }

    pub fn unsubscribe(&self, r: &ResourceRef) {
        let mut refs = self.refs.lock().unwrap();
        refs.retain(|_, aliases| {
            aliases.remove(r);
            !aliases.is_empty()
        });
    }

    pub fn aliases(&self, kind: ResourceKind, project: &str, id: &str) -> Vec<ResourceRef> {
        let refs = self.refs.lock().unwrap();
        let mut out: HashSet<ResourceRef> = refs
            .get(&(kind, project.to_string(), id.to_string()))
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        out.extend(
            refs.get(&(kind, project.to_string(), "*".to_string()))
                .into_iter()
                .flatten()
                .cloned(),
        );
        out.into_iter().collect()
    }

    pub fn remove_session(&self, session: &str) {
        let mut refs = self.refs.lock().unwrap();
        refs.retain(|_, aliases| {
            aliases.retain(|r| r.location.session != session);
            !aliases.is_empty()
        });
    }

    pub fn remove_resource(&self, kind: ResourceKind, project: &str, id: &str) {
        self.refs
            .lock()
            .unwrap()
            .remove(&(kind, project.to_string(), id.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_cannot_cross_resource_classes() {
        assert!(ResourceOperationKind::FileRead.accepts(ResourceKind::File));
        assert!(!ResourceOperationKind::FileRead.accepts(ResourceKind::Terminal));
        assert!(ResourceOperationKind::BrowserKey.accepts(ResourceKind::Browser));
        assert!(!ResourceOperationKind::Extension.accepts(ResourceKind::Project));
    }

    #[test]
    fn an_old_reference_without_authority_stays_chat_scoped() {
        let reference: ResourceRef = serde_json::from_value(serde_json::json!({
            "location": { "runtime": "runtime", "project": "project", "session": "chat" },
            "kind": "terminal",
            "id": "term-old"
        }))
        .unwrap();
        assert_eq!(reference.authority, ResourceAuthority::Chat);
    }

    #[test]
    fn shared_resources_emit_one_alias_per_chat_and_unsubscribe_cleanly() {
        let subscribers = ResourceSubscribers::default();
        let reference = |session: &str| ResourceRef {
            location: ResourceLocation {
                runtime: "runtime".into(),
                project: "project".into(),
                session: session.into(),
            },
            kind: ResourceKind::Process,
            id: "process".into(),
            authority: ResourceAuthority::Chat,
        };
        let (one, two) = (reference("one"), reference("two"));
        subscribers.subscribe(ResourceKind::Process, "project", "process", one.clone());
        subscribers.subscribe(ResourceKind::Process, "project", "*", one.clone());
        subscribers.subscribe(ResourceKind::Process, "project", "*", two.clone());
        let aliases = subscribers.aliases(ResourceKind::Process, "project", "process");
        assert_eq!(
            aliases.len(),
            2,
            "an exact and wildcard subscription do not duplicate"
        );
        assert!(aliases.contains(&one) && aliases.contains(&two));
        subscribers.unsubscribe(&one);
        assert_eq!(
            subscribers.aliases(ResourceKind::Process, "project", "process"),
            vec![two.clone()]
        );
        subscribers.remove_session("two");
        assert!(subscribers
            .aliases(ResourceKind::Process, "project", "process")
            .is_empty());
    }
}
