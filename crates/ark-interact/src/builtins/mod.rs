//! The generic commands every ARK app has. Each maps onto a real runtime capability and
//! returns a [`Report`](crate::report::Report); none prints anything.

use std::sync::Arc;

use crate::command::Command;

macro_rules! meta {
    ($name:literal, $summary:literal) => {
        fn name(&self) -> &str {
            $name
        }
        fn summary(&self) -> &str {
            $summary
        }
        fn group(&self) -> &str {
            $crate::builtins::SESSION_GROUP
        }
    };
}

mod session;
mod views;

/// The group the generic commands are listed under in `/help`.
pub const SESSION_GROUP: &str = "Session";

pub(crate) fn all() -> Vec<Arc<dyn Command>> {
    vec![
        Arc::new(session::Help),
        Arc::new(session::New),
        Arc::new(session::Resume),
        Arc::new(session::Sessions),
        Arc::new(session::Rename),
        Arc::new(session::ProjectCmd),
        Arc::new(session::Folder),
        Arc::new(session::Model),
        Arc::new(views::Context),
        Arc::new(views::Usage),
        Arc::new(views::Tools),
        Arc::new(views::Plugin),
        Arc::new(views::Skills),
        Arc::new(views::TraceCmd),
        Arc::new(views::State),
        Arc::new(views::Status),
        Arc::new(session::Compact),
        Arc::new(session::Clear),
        Arc::new(views::Config),
        Arc::new(session::Debug),
        Arc::new(session::Exit),
    ]
}

pub use views::{call_report, trace_report};

/// `/agents`, `/agent` and `/task`, offered when the app has an orchestrator.
pub(crate) fn agents() -> Vec<Arc<dyn Command>> {
    vec![
        Arc::new(views::Agents),
        Arc::new(views::AgentCmd),
        Arc::new(views::TaskCmd),
    ]
}
