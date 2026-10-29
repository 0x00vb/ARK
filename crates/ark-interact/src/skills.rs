//! Skills as both front-ends show them: `/skills` in a terminal, the agent inspector in a
//! window. Finding and reading skills is the runtime's ([`ark_sdk::prelude::ark_runtime::skills`]);
//! this module only lists them for a person.

use ark_sdk::prelude::ark_core::ProjectId;
use ark_sdk::prelude::ark_runtime::skills::{self, Skill, SkillScope};
use ark_sdk::prelude::Engine;
use ark_sdk::Ark;
use serde::Serialize;

use crate::report::{Cell, Report, Row, Table, Tone};

/// One skill as a front-end lists it.
#[derive(Clone, Debug, Serialize)]
pub struct SkillView {
    pub name: String,
    pub description: String,
    pub scope: SkillScope,
    /// The conversation may read it (agents choose their own).
    pub chat: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
}

/// Every skill of the project folder and the store, project first, and whether the
/// conversation may read each.
pub fn list(ark: &Ark, project: &ProjectId) -> Vec<SkillView> {
    list_for(ark.engine(), project, ark.skill_names())
}

/// [`list`] from the engine and the names the conversation's skills are chosen by, for a
/// front-end that should not wait for a running turn.
pub fn list_for(engine: &Engine, project: &ProjectId, chat: &[String]) -> Vec<SkillView> {
    let packages = match skills::packages::directories(engine) {
        Ok(packages) => packages,
        Err(error) => {
            tracing::warn!(%error, "Installed skill ownership could not be read");
            Vec::new()
        }
    };
    all(engine, project)
        .into_iter()
        .map(|s| SkillView {
            package: packages
                .iter()
                .find(|(_, root)| s.dir.starts_with(root))
                .map(|(id, _)| id.clone()),
            chat: chat.iter().any(|p| p == "*" || p == &s.name),
            name: s.name,
            description: s.description,
            scope: s.scope,
        })
        .collect()
}

fn all(engine: &Engine, project: &ProjectId) -> Vec<Skill> {
    skills::discover(&skills::dirs(engine, project))
}

/// The skills as a table, and where to put more.
pub fn report(list: &[SkillView], engine: &Engine, project: &ProjectId) -> Report {
    let places: Vec<String> = skills::dirs(engine, project)
        .into_iter()
        .filter(|(_, scope)| *scope != SkillScope::Builtin)
        .map(|(p, _)| p.display().to_string())
        .collect();
    if list.is_empty() {
        return Report::new().note(format!(
            "No skills. A skill is a folder with a SKILL.md (a name, a description, then the instructions) in {}.",
            places.join(" or ")
        ));
    }
    let mut t = Table::plain(3);
    for s in list {
        let scope = match s.scope {
            SkillScope::Project => "project",
            SkillScope::Store => "all projects",
            SkillScope::Builtin => "built in",
        };
        t.push(Row {
            cells: vec![
                Cell::new(s.name.clone(), Tone::Normal),
                Cell::new(scope, Tone::Accent),
                Cell::new(s.description.clone(), Tone::Muted),
            ],
            tone: Tone::Normal,
            command: None,
        });
    }
    Report::new().table(t).note(format!(
        "The conversation reads them when a task fits; an agent reads the ones it was given. More go in {}.",
        places.join(" or ")
    ))
}

pub fn packages(engine: &Engine) -> anyhow::Result<Vec<skills::packages::PackageView>> {
    Ok(skills::packages::list(engine)?)
}

pub async fn install_package(
    engine: std::sync::Arc<Engine>,
    project: ProjectId,
    id: String,
    digest: String,
) -> anyhow::Result<skills::packages::PackageView> {
    Ok(tokio::task::spawn_blocking(move || {
        skills::packages::install(&engine, &project, &id, &digest)
    })
    .await??)
}

pub async fn remove_package(engine: std::sync::Arc<Engine>, id: String) -> anyhow::Result<()> {
    Ok(tokio::task::spawn_blocking(move || skills::packages::remove(&engine, &id)).await??)
}
