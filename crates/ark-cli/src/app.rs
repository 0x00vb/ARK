//! Shell state shared by the interactive loop, the turn driver and headless runs. Everything
//! that is not drawing lives in the [`Interaction`].

use std::path::PathBuf;
use std::sync::Arc;

use ark_interact::{Interaction, PendingApproval};
use ark_sdk::prelude::*;
use tokio::sync::mpsc;

use crate::term::Out;
use crate::theme::Theme;

/// Renders a runtime event the consumer cares about (e.g. a `Present` directive) as one line.
/// Return `None` to use the default rendering.
pub type EventHook = Arc<dyn Fn(&HarnessEvent, &Theme) -> Option<String> + Send + Sync>;

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
    pub bin: String,
    pub theme: Theme,
    pub welcome: Option<WelcomeFn>,
    pub event_hook: Option<EventHook>,
    pub history_file: Option<PathBuf>,
}

pub(crate) struct ShellState {
    pub cfg: Arc<Config>,
    pub ix: Interaction,
    pub approvals: mpsc::UnboundedReceiver<PendingApproval>,
    pub out: Out,
}

impl ShellState {
    /// Print a command's report.
    pub(crate) fn print(&mut self, report: &ark_interact::Report) {
        let width = self.out.width();
        for l in crate::render::render(report, &self.cfg.theme, width) {
            self.out.line(&l);
        }
    }
}
