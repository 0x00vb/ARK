//! Shell state shared by the interactive loop, the turn driver and slash commands.

use std::path::PathBuf;
use std::sync::Arc;

use ark_sdk::prelude::*;
use tokio::sync::mpsc;

use crate::activity::Activity;
use crate::approval::Pending;
use crate::commands::Command;
use crate::term::Out;
use crate::theme::Theme;

/// Renders a runtime event the consumer cares about (e.g. a `Present` directive) as one line.
/// Return `None` to use the default rendering.
pub type EventHook = Arc<dyn Fn(&HarnessEvent, &Theme) -> Option<String> + Send + Sync>;

/// Contributes a titled block to `/status`. Return `None` to hide it.
pub type StatusFn = Arc<dyn Fn(&Ark, &Project) -> Option<String> + Send + Sync>;

/// Builds the welcome banner.
pub type WelcomeFn = Arc<dyn Fn(&Welcome, &Theme) -> String + Send + Sync>;

/// What a welcome banner may show.
pub struct Welcome {
    pub name: String,
    pub tagline: Option<String>,
    pub model: String,
    pub project: String,
    pub session: String,
    pub resumed: bool,
    pub turns: u32,
}

pub(crate) struct Config {
    pub name: String,
    pub bin: String,
    pub tagline: Option<String>,
    pub theme: Theme,
    pub welcome: Option<WelcomeFn>,
    pub help_intro: Option<String>,
    pub activity: Activity,
    pub event_hook: Option<EventHook>,
    pub status: Vec<(String, StatusFn)>,
    pub commands: Vec<Arc<dyn Command>>,
    pub history_file: Option<PathBuf>,
}

impl Config {
    pub(crate) fn find_command(&self, name: &str) -> Option<Arc<dyn Command>> {
        self.commands
            .iter()
            .find(|c| c.name() == name || c.aliases().contains(&name))
            .cloned()
    }
}

/// One finished turn, kept for `/trace`.
pub struct TurnRecord {
    pub input: String,
    /// (milliseconds since the turn began, event). Text deltas are merged per step.
    pub events: Vec<(u64, HarnessEvent)>,
    pub elapsed_ms: u64,
    pub cancelled: bool,
    pub error: Option<String>,
}

pub(crate) struct ShellState {
    pub cfg: Arc<Config>,
    pub ark: Ark,
    pub project: Project,
    pub session: Session,
    pub gate: Arc<ApprovalGate>,
    pub approvals: mpsc::UnboundedReceiver<Pending>,
    pub out: Out,
    pub last_turn: Option<TurnRecord>,
    pub last_context: Option<ContextReport>,
    pub verbose: bool,
}
