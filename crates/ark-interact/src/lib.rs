//! # ark-interact
//!
//! The interaction layer between the ARK runtime and any front-end. The runtime reasons, calls
//! tools and manages context; a front-end draws. In between sits everything a person does with
//! an agent that is not drawing:
//!
//! - [`App`]: an application described once (name, agent, commands, labels, status).
//! - [`Interaction`]: one live connection to it: project, session, approvals, turns.
//! - [`Command`] and [`Reply`]: slash commands that return structured [`Report`]s.
//! - [`Labels`] and [`activity`]: runtime events in a person's words.
//! - [`trace`]: a turn's events, interpreted.
//! - [`inspect`]: typed views of context, usage, working state and sessions.
//! - [`ChannelApprover`]: carries approval questions to the front-end.
//! - [`orchestration`]: the agent and task graph of an [`App::orchestrator`], as reports.
//! - [`remote`]: a client of a runtime served elsewhere.
//!
//! `ark-cli` and `ark-ui` both build on this crate and on nothing of each other. It knows
//! nothing about terminals, windows, HTML or ANSI codes.

pub mod activity;
pub mod approval;
mod builtins;
pub mod capabilities;
pub mod command;
pub mod fmt;
pub mod inspect;
pub mod interaction;
pub mod labels;
pub mod logging;
pub mod orchestration;
pub mod plugins;
pub mod remote;
pub mod report;
pub mod skills;
pub mod trace;

pub use activity::{Activity, ActivityKind};
pub use approval::{approval_choices, approval_title, ChannelApprover, Choice, PendingApproval};
pub use builtins::{call_report, trace_report, SESSION_GROUP};
pub use command::{Command, CommandInfo, Effect, Reply};
pub use inspect::Pricing;
pub use interaction::{App, Interaction, Opened, Siblings, Start, Summary, TurnControl, TurnEnd};
pub use labels::{default_label, ActivityLabels, Labels, ToolLabeler};
pub use report::{Block, Report, Tone};
pub use trace::{Trace, TurnRecord};

pub use ark_orchestrate;
pub use ark_sdk;
