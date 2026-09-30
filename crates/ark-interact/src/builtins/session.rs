//! Commands that move between sessions, projects and models, or change what the front-end
//! shows.

use ark_sdk::prelude::*;

use super::SESSION_GROUP;
use crate::command::{Command, Effect, Reply};
use crate::inspect::sessions_report;
use crate::interaction::Interaction;
use crate::report::{Cell, Report, Row, Table, Tone};

pub struct Help;
#[async_trait::async_trait]
impl Command for Help {
    meta!("help", "Show commands and keys");
    fn aliases(&self) -> &[&str] {
        &["?"]
    }
    async fn run(&self, ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        let mut r = Report::new();
        if let Some(intro) = &ix.info().help {
            r = r.text(intro.clone());
        }
        let cmds = ix.commands();
        let mut groups: Vec<String> = Vec::new();
        for c in &cmds {
            if !groups.contains(&c.group) {
                groups.push(c.group.clone());
            }
        }
        // Domain groups first, the generic ones after.
        groups.sort_by_key(|g| g == SESSION_GROUP);
        for g in groups {
            let mut t = Table::plain(2);
            for c in cmds.iter().filter(|c| c.group == g) {
                t.push(Row {
                    cells: vec![
                        Cell::new(format!("/{} {}", c.name, c.usage).trim_end(), Tone::Accent),
                        Cell::new(c.summary.clone(), Tone::Muted),
                    ],
                    tone: Tone::Normal,
                    command: Some(format!("/{}", c.name)),
                });
            }
            r = r.heading(g).table(t);
        }
        if !ix.keys().is_empty() {
            let mut t = Table::plain(2);
            for (k, v) in ix.keys() {
                t.push(Row {
                    cells: vec![
                        Cell::new(k.clone(), Tone::Accent),
                        Cell::new(v.clone(), Tone::Muted),
                    ],
                    tone: Tone::Normal,
                    command: None,
                });
            }
            r = r.heading("Keys").table(t);
        }
        Ok(r.data("commands", &cmds).into())
    }
}

pub struct New;
#[async_trait::async_trait]
impl Command for New {
    meta!(
        "new",
        "Start a new session (working state and evidence are kept)"
    );
    async fn run(&self, ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        ix.new_session()?;
        Ok(Report::new()
            .success(format!("new session {}", ix.session().id().short()))
            .into())
    }
}

pub struct Resume;
#[async_trait::async_trait]
impl Command for Resume {
    meta!("resume", "Continue an earlier session");
    fn usage(&self) -> &str {
        "[session]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let target = if args.is_empty() {
            ix.sessions(false)?
                .into_iter()
                .find(|s| !s.current && s.turns > 0)
                .map(|s| s.id)
        } else {
            Some(args.to_string())
        };
        let Some(target) = target else {
            return Ok(Report::new()
                .note("No other session to resume. /sessions lists them.")
                .into());
        };
        if !ix.resume(&target)? {
            return Ok(Report::new()
                .warning(format!(
                    "No session matches `{target}`. /sessions lists them."
                ))
                .into());
        }
        Ok(ix.recap().into())
    }
}

pub struct Sessions;
#[async_trait::async_trait]
impl Command for Sessions {
    meta!("sessions", "List sessions in this project");
    async fn run(&self, ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        Ok(sessions_report(&ix.sessions(false)?).into())
    }
}

pub struct Rename;
#[async_trait::async_trait]
impl Command for Rename {
    meta!("rename", "Give this session a title");
    fn usage(&self) -> &str {
        "<title>"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        if args.trim().is_empty() {
            return Ok(Report::new().warning("usage: /rename <title>").into());
        }
        ix.rename_session(None, args)?;
        Ok(Report::new()
            .success(format!("renamed to {}", ix.session().record().title))
            .into())
    }
}

pub struct ProjectCmd;
#[async_trait::async_trait]
impl Command for ProjectCmd {
    meta!(
        "project",
        "List projects, or switch: /project <name> (created if new)"
    );
    fn usage(&self) -> &str {
        "[name]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        if args.trim().is_empty() {
            let mut t = Table::plain(3);
            for p in ix.projects()? {
                t.push(Row {
                    cells: vec![
                        Cell::new(if p.current { "›" } else { " " }, Tone::Accent),
                        Cell::new(p.name.clone(), Tone::Normal),
                        Cell::new(p.target.clone(), Tone::Muted),
                    ],
                    tone: Tone::Normal,
                    command: Some(format!("/project {}", p.name)),
                });
            }
            return Ok(Report::new().table(t).into());
        }
        let p = ix.switch_project(args)?;
        Ok(Report::new()
            .success(format!("now in project {}", p.name))
            .into())
    }
}

pub struct Model;
#[async_trait::async_trait]
impl Command for Model {
    meta!(
        "model",
        "Show the model, or switch: /model <model> or /model <provider> <model>"
    );
    fn usage(&self) -> &str {
        "[provider] [model]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        if args.trim().is_empty() {
            let router = ix.ark().router();
            return Ok(Report::new()
                .field("reasoning", router.provider(Role::Reason).describe())
                .field("planning", router.provider(Role::Plan).describe())
                .field("routing", router.describe())
                .into());
        }
        Ok(match ix.switch_model(args) {
            Ok(d) => Report::new().success(format!("now using {d}")),
            Err(e) => Report::new().error(format!("Could not switch model: {e}")),
        }
        .into())
    }
}

pub struct Compact;
#[async_trait::async_trait]
impl Command for Compact {
    meta!(
        "compact",
        "Tighten the carried context (keeps goals, findings, open questions)"
    );
    async fn run(&self, ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        let r = ix.compact()?;
        Ok(Report::new()
            .success("compacted")
            .field(
                "conversation",
                format!(
                    "{} → {} tokens ({} turns folded, {} gists merged)",
                    r.conversation_before, r.conversation_after, r.turns_folded, r.gists_dropped
                ),
            )
            .field(
                "working state",
                format!(
                    "{} → {} tokens ({} stale or duplicate items removed)",
                    r.state_before, r.state_after, r.state_items_removed
                ),
            )
            .note("Nothing was deleted: the transcript and all evidence remain retrievable.")
            .data("compact", &r)
            .into())
    }
}

pub struct Clear;
#[async_trait::async_trait]
impl Command for Clear {
    meta!("clear", "Clear the screen (the conversation is kept)");
    async fn run(&self, _ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        Ok(Reply::none().effect(Effect::ClearView))
    }
}

pub struct Debug;
#[async_trait::async_trait]
impl Command for Debug {
    meta!(
        "debug",
        "Toggle the detailed view (arguments, durations, model calls)"
    );
    fn usage(&self) -> &str {
        "[on|off]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let on = match args.trim() {
            "on" => true,
            "off" => false,
            _ => !ix.detail(),
        };
        ix.set_detail(on);
        Ok(Report::new()
            .note(if on {
                "debug view on"
            } else {
                "debug view off"
            })
            .into())
    }
}

pub struct Exit;
#[async_trait::async_trait]
impl Command for Exit {
    meta!("exit", "Leave (the session is saved; resume with --resume)");
    fn aliases(&self) -> &[&str] {
        &["quit"]
    }
    async fn run(&self, _ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        Ok(Reply::none().effect(Effect::Exit))
    }
}
