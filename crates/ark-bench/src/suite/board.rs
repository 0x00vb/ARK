//! The `board` workload: the person's conversation plans a three-part piece of work on the
//! project's board and hands the first part to a writer. After a restart, a new conversation
//! (nothing of the first one in view) continues with the next part. A run succeeds when the
//! board holds the three parts once each, the first done by the writer, exactly one other part
//! handed off and done, and the third still to do: the board carried the work across the
//! restart and the new chat, and nothing was done twice.

use ark_orchestrate::{AgentSpec, Orchestrator, WorkState};
use ark_sdk::prelude::*;
use serde_json::json;
use std::sync::Arc;

use super::{housekeeping, Spec};
use crate::sim::{Policy, Reply, SimRequest};
use crate::workload::{Env, Mode, RunOutput, TurnSpec, Verdict, Workload};

const PLAN: &str = "We are writing a short guide for new volunteers at a community garden, in three parts: a welcome page, a watering schedule and a composting page. Put the three parts on the project's board, then have the writer draft the welcome page.";
const NEXT: &str = "Continue the volunteer guide: hand the next part on the board to the writer.";

/// The items a result names, in order: (id, the text up to the next id). Works on an evidence
/// card (`w-1a2b3c4d [todo] Welcome page`) and on raw JSON (`"item": "w-…", "title": …`).
fn items_in(text: &str) -> Vec<(String, String)> {
    let starts: Vec<usize> = text
        .match_indices("w-")
        .map(|(i, _)| i)
        .filter(|&i| {
            let id = &text[i + 2..];
            id.len() >= 8 && id.chars().take(8).all(|c| c.is_ascii_hexdigit())
        })
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(n, &i)| {
            let end = starts.get(n + 1).copied().unwrap_or(text.len());
            (
                text[i..i + 10].to_string(),
                text[i + 10..end].to_lowercase(),
            )
        })
        .collect()
}

fn delegate(id: &str, objective: &str) -> Reply {
    Reply::call(
        "agents.delegate",
        json!({ "to": "writer", "objective": objective, "item": id }),
    )
}

/// A model that plans on the board and hands off by item, the way the tools describe it.
fn policy() -> Policy {
    Arc::new(|r: &SimRequest| {
        if let Some(reply) = housekeeping(r) {
            return reply;
        }
        // A writer's task: it drafts and answers.
        if !r.has_tool("board.add") {
            return Reply::Text(format!(
                "Drafted: {}",
                r.task().lines().last().unwrap_or("")
            ));
        }
        let first_turn = r.task().contains("three parts");
        if first_turn && r.count_calls("board.add") == 0 {
            return Reply::call(
                "board.add",
                json!({ "items": [
                    { "title": "Welcome page" },
                    { "title": "Watering schedule" },
                    { "title": "Composting page" }
                ] }),
            );
        }
        if !first_turn && r.count_calls("board.list") == 0 {
            return Reply::call("board.list", json!({}));
        }
        if r.count_calls("agents.delegate") == 0 {
            let seen = if first_turn {
                r.result_of("board.add")
            } else {
                r.result_of("board.list")
            }
            .unwrap_or("");
            let pick = items_in(seen).into_iter().find(|(_, rest)| {
                if first_turn {
                    rest.contains("welcome")
                } else {
                    rest.contains("todo") && !rest.contains("done")
                }
            });
            if let Some((id, rest)) = pick {
                let title = rest
                    .split(['\n', '"'])
                    .find(|s| s.chars().any(char::is_alphabetic) && !s.contains("todo"))
                    .unwrap_or("the next part")
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_string();
                return delegate(&id, &format!("Draft the {title}"));
            }
        }
        Reply::Text(if first_turn {
            "The three parts are on the board; the writer drafted the welcome page.".into()
        } else {
            "The writer drafted the next part.".into()
        })
    })
}

/// A team workload whose turns go through the person's conversation.
pub struct BoardSpec {
    spec: Spec,
}

impl Workload for BoardSpec {
    fn name(&self) -> &str {
        self.spec.name
    }
    fn class(&self) -> &str {
        self.spec.class
    }
    fn description(&self) -> &str {
        self.spec.description
    }
    fn turns(&self, mode: Mode) -> Vec<TurnSpec> {
        (self.spec.turns)(mode)
    }
    fn policy(&self) -> Policy {
        self.spec.policy.clone()
    }
    fn build(&self, env: &Env) -> anyhow::Result<Ark> {
        Ok(env.builder().build()?)
    }
    fn prepare(&self, ark: &Ark) -> anyhow::Result<ProjectId> {
        Ok(ark.find_or_create_project("bench", "")?.id)
    }
    fn check(&self, out: &RunOutput) -> Verdict {
        (self.spec.check)(out)
    }
    fn team(&self) -> Vec<AgentSpec> {
        vec![AgentSpec::new("writer").role("drafts the pages of the guide")]
    }
    fn chat(&self) -> bool {
        true
    }
}

fn check(o: &RunOutput) -> Verdict {
    let board = (|| {
        let p = o.ark.engine().store.get_project(o.project).ok()??;
        Some(Orchestrator::builder(o.ark, &p).build().ok()?.board())
    })()
    .unwrap_or_default();
    let welcome: Vec<_> = board
        .iter()
        .filter(|w| w.title.to_lowercase().contains("welcome"))
        .collect();
    let others: Vec<_> = board
        .iter()
        .filter(|w| !w.title.to_lowercase().contains("welcome"))
        .collect();
    let handed: Vec<_> = others.iter().filter(|w| !w.tasks.is_empty()).collect();
    let left: Vec<_> = others.iter().filter(|w| w.tasks.is_empty()).collect();
    Verdict::all(&[
        (
            board.len() == 3,
            "the board does not hold the three parts once each",
        ),
        (
            welcome.len() == 1
                && welcome[0].state == WorkState::Done
                && welcome[0].owner.as_deref() == Some("writer")
                && welcome[0].tasks.len() == 1,
            "the welcome page was not handed off once and done",
        ),
        (
            handed.len() == 1 && handed[0].state == WorkState::Done && handed[0].tasks.len() == 1,
            "the new chat did not hand off exactly one more part",
        ),
        (
            left.len() == 1 && left[0].state == WorkState::Todo,
            "the third part is not still to do",
        ),
    ])
}

pub fn board() -> BoardSpec {
    BoardSpec {
        spec: Spec {
            name: "board",
            class: "orchestration",
            description: "the chat plans three parts on the board and hands one off; after a restart a new chat hands off the next",
            turns: Arc::new(|_| {
                vec![
                    TurnSpec::new(PLAN),
                    TurnSpec::new_chat_after_restart(NEXT),
                ]
            }),
            policy: policy(),
            prepare: None,
            check: Arc::new(check),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_read_from_cards_and_from_json() {
        let card = "added 2 items to the board:\nw-1a2b3c4d [todo] Welcome page\nw-5e6f7a8b [done, writer] Watering schedule";
        let v = items_in(card);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, "w-1a2b3c4d");
        assert!(v[0].1.contains("welcome") && v[1].1.contains("done"));
        let raw = r#"{"items":[{"item":"w-0000aaaa","title":"Composting page","state":"todo"}]}"#;
        let v = items_in(raw);
        assert_eq!(v[0].0, "w-0000aaaa");
        assert!(v[0].1.contains("composting") && v[0].1.contains("todo"));
    }
}
