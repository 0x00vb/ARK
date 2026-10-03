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
}

/// Every skill of the project folder and the store, project first, and whether the
/// conversation may read each.
pub fn list(ark: &Ark, project: &ProjectId) -> Vec<SkillView> {
    list_for(ark.engine(), project, ark.skill_names())
}

/// [`list`] from the engine and the names the conversation's skills are chosen by, for a
/// front-end that should not wait for a running turn.
pub fn list_for(engine: &Engine, project: &ProjectId, chat: &[String]) -> Vec<SkillView> {
    all(engine, project)
        .into_iter()
        .map(|s| SkillView {
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
