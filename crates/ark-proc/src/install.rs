//! Programs a process needs and ARK can install, through the request flow of ADR 0013: the
//! agent calls `acquire_capability`, the person sees "Install Node.js" with the exact command,
//! and the install runs as a process they can watch. The ways to install are data
//! (`tools.json`): Homebrew where it is installed, else the tool's own installer.

use std::path::PathBuf;
use std::time::Duration;

use ark_runtime::{AcquisitionProvider, AcquisitionResult};
use serde::Deserialize;

use crate::{Processes, Shell, Start, State};

/// The longest an install may take.
const INSTALL_TIME: Duration = Duration::from_secs(20 * 60);

/// A program ARK can install.
#[derive(Clone, Debug, Deserialize)]
pub struct Tool {
    pub id: String,
    pub label: String,
    /// What it lets ARK do, for the person.
    pub says: String,
    /// The programs it brings (`npm`, `npx`).
    pub programs: Vec<String>,
    /// Ways to install it, in order: the first whose `needs` is installed is used.
    pub install: Vec<Way>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Way {
    /// A program this way runs (`brew`, `curl`).
    pub needs: Option<String>,
    /// The command line, run in the person's home folder.
    pub run: String,
}

/// The programs ARK knows how to install.
pub struct Catalog(Vec<Tool>);

impl Catalog {
    pub fn builtin() -> Catalog {
        Catalog(serde_json::from_str(include_str!("tools.json")).expect("tools.json parses"))
    }

    /// The tool that brings `program`.
    pub fn for_program(&self, program: &str) -> Option<&Tool> {
        let name = program.rsplit('/').next().unwrap_or(program);
        self.0.iter().find(|t| t.programs.iter().any(|p| p == name))
    }

    /// The tool a free-text need names: its id, its label or one of its programs.
    pub fn for_need(&self, need: &str) -> Option<&Tool> {
        let need = need.trim().to_lowercase();
        let words: Vec<&str> = need
            .split(|c: char| !(c.is_alphanumeric() || c == '.' || c == '-'))
            .filter(|w| !w.is_empty())
            .collect();
        self.0.iter().find(|t| {
            need == t.id
                || need == t.label.to_lowercase()
                || words.iter().any(|w| {
                    *w == t.id || *w == t.label.to_lowercase() || t.programs.iter().any(|p| p == w)
                })
        })
    }
}

/// What an install of a tool would run here.
pub struct Plan {
    pub tool: Tool,
    /// `None`: no way to install it on this computer.
    pub command: Option<String>,
    pub installed: bool,
}

/// The installs, as an acquisition source.
#[derive(Clone)]
pub struct Installs {
    processes: Processes,
}

impl Installs {
    pub fn new(processes: Processes) -> Installs {
        Installs { processes }
    }

    /// What installing the tool `need` names would do, if ARK knows it.
    pub async fn plan(&self, need: &str) -> Option<Plan> {
        let tool = self.processes.installs().for_need(need)?.clone();
        let env = self.processes.shell().env().await;
        let home = home();
        let has = |p: &str| Shell::find(p, &env, &home).is_some();
        let installed = tool.programs.iter().any(|p| has(p));
        let command = tool
            .install
            .iter()
            .find(|w| w.needs.as_deref().is_none_or(has))
            .map(|w| w.run.clone());
        Some(Plan {
            tool,
            command,
            installed,
        })
    }

    /// What installing the program `command` runs would do, when ARK installs that program.
    /// A path (`./bin/server`, `/opt/bun/bin/bun`) is not one: ARK installs into the PATH.
    pub async fn plan_for_command(&self, command: &str) -> Option<Plan> {
        let program = crate::shell::program_of(command).filter(|p| !p.contains('/'))?;
        let id = self.processes.installs().for_program(&program)?.id.clone();
        self.plan(&id).await
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[async_trait::async_trait]
impl AcquisitionProvider for Installs {
    fn knows(&self, need: &str) -> bool {
        self.processes.installs().for_need(need).is_some()
    }

    async fn find(&self, need: &str) -> String {
        match self.plan(need).await {
            Some(p) if p.installed => format!("{} is installed.", p.tool.label),
            Some(Plan {
                tool,
                command: Some(cmd),
                ..
            }) => format!(
                "{} is not installed. acquire_capability with need \"{}\" asks the person to install it (`{cmd}`).",
                tool.label, tool.id
            ),
            Some(p) => format!(
                "{} is not installed, and this computer has no way ARK knows to install it.",
                p.tool.label
            ),
            None => format!("no program ARK installs matches `{need}`."),
        }
    }

    async fn propose(&self, need: &str) -> String {
        match self.plan(need).await {
            Some(Plan {
                tool,
                command: Some(cmd),
                ..
            }) => format!(
                "Installing {} runs `{cmd}` on this computer, as the person, after they allow it. They can watch it in Processes.",
                tool.label
            ),
            Some(p) => format!("ARK has no way to install {} here.", p.tool.label),
            None => format!("no program ARK installs matches `{need}`."),
        }
    }

    async fn acquire(&self, need: &str, approved: bool) -> AcquisitionResult {
        let Some(plan) = self.plan(need).await else {
            return AcquisitionResult::info(format!("no program ARK installs matches `{need}`."));
        };
        let label = plan.tool.label.clone();
        if plan.installed {
            return AcquisitionResult::changed(format!(
                "{label} is installed. Start the process again."
            ));
        }
        if !approved {
            return AcquisitionResult::info(format!(
                "The person did not install {label}. Do what you can without it and say what is missing."
            ));
        }
        let Some(command) = plan.command else {
            return AcquisitionResult::info(format!(
                "ARK has no way to install {label} on this computer (it needs Homebrew or curl). Tell the person what is missing."
            ));
        };
        // A new attempt the person allowed replaces what the last one left.
        self.processes.clear_install_failure(&plan.tool.id);
        let start = Start {
            command,
            ..Start::default()
        };
        let p = match self.processes.start(&home(), &start).await {
            Ok(p) => p,
            Err(e) => return AcquisitionResult::info(format!("{label} was not installed: {e}")),
        };
        let ended = self.processes.wait_end(&p.id, INSTALL_TIME).await;
        self.processes.shell().refresh().await;
        match self.plan(need).await {
            Some(now) if now.installed => {
                self.processes.clear_install_failure(&now.tool.id);
                AcquisitionResult::changed(format!(
                    "{label} is installed. Start the process again."
                ))
            }
            _ => {
                let how = match ended {
                    Some(i) if i.state == State::Exited => {
                        format!("it ended with exit {}", i.code.unwrap_or(-1))
                    }
                    Some(i) if i.state == State::Stopped => "it was stopped".into(),
                    _ => "it did not finish in time".into(),
                };
                let tail = self.processes.output(&p.id, 1500).unwrap_or_default();
                self.processes.record_install_failure(
                    &plan.tool.id,
                    format!("{how}. The end of its output:\n{tail}"),
                );
                AcquisitionResult::info(format!(
                    "{label} was not installed: {how}. The end of its output:\n{tail}"
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_name_a_tool_by_id_label_or_program() {
        let c = Catalog::builtin();
        assert_eq!(c.for_need("node").unwrap().id, "node");
        assert_eq!(c.for_need("Node.js").unwrap().id, "node");
        assert_eq!(c.for_need("npm").unwrap().id, "node");
        assert_eq!(c.for_need("install pnpm please").unwrap().id, "pnpm");
        assert!(c.for_need("photoshop").is_none());
        assert_eq!(c.for_program("/usr/local/bin/cargo").unwrap().id, "rust");
    }
}
