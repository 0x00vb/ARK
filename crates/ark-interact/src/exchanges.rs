//! Exchanges: what agents said to each other, shaped for any front-end (P2 of the agent team
//! plan). A chat shows one quiet line for each of its turns that reached other agents; the
//! line opens the messages between two of them. Nothing is stored for this: the read is
//! derived from the notes, briefs and replies the orchestrator already keeps
//! ([`ark_orchestrate::said`]).

use std::collections::HashMap;

use ark_orchestrate::{known, said, Known, OrchError, Said};
use ark_sdk::prelude::ark_runtime::Engine;
use serde::Serialize;

/// Messages one exchange returns (the newest).
pub const MAX_MESSAGES: usize = 200;
/// Characters of one message an exchange returns.
pub const MAX_CHARS: usize = 2000;

/// An agent as a line or a header names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Who {
    /// The agent's name, the id a front-end opens.
    pub agent: String,
    /// What the person sees: the title they gave it, else its name.
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub look: String,
    /// In a line: its reply never came.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}

/// One piece of a line's words: plain text, or the `with` agent at that index (a front-end
/// draws its pet there).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    Text(String),
    With(usize),
}

/// What one turn of a chat started with other agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Line {
    /// The turn: the id of the task that made it.
    pub turn: String,
    /// The agents asked, the first asked first.
    pub with: Vec<Who>,
    /// Messages in it: notes, briefs and replies.
    pub count: usize,
    pub last_ms: u64,
    /// A reply that never came.
    pub failed: bool,
    /// The words as plain text.
    pub label: String,
    /// The same words with the agents' slots.
    pub parts: Vec<Part>,
}

/// One message of an exchange.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Message {
    /// The agent that wrote it.
    pub author: String,
    pub text: String,
    pub at_ms: u64,
    /// The task that carried it when it is a brief or its reply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
    /// A brief whose reply may still come.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

/// The messages between two agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Talk {
    pub with: [Who; 2],
    pub messages: Vec<Message>,
}

fn who(names: &HashMap<String, Known>, agent: &str) -> Who {
    let known = names.get(agent);
    let title = known.map(|k| k.title.trim()).filter(|t| !t.is_empty());
    Who {
        agent: agent.to_string(),
        name: title.unwrap_or(agent).to_string(),
        look: known.map(|k| k.look.clone()).unwrap_or_default(),
        failed: false,
    }
}

fn text(s: &str) -> Part {
    Part::Text(s.to_string())
}

/// The words of a line, with a slot where each agent goes. `failed` are the slots whose reply
/// never came; every agent asked stays in the words.
fn words(count: usize, with: &[usize], failed: &[usize]) -> Vec<Part> {
    let slots = |names: &[usize]| -> Vec<Part> {
        let mut out = Vec::new();
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                out.push(text(if i + 1 == names.len() { " and " } else { ", " }));
            }
            out.push(Part::With(*n));
        }
        out
    };
    let missing = |mut out: Vec<Part>, names: &[usize]| {
        out.push(text("Reply from "));
        out.extend(slots(names));
        out.push(text(" did not arrive"));
        out
    };
    if !failed.is_empty() && failed.len() == with.len() {
        return missing(Vec::new(), with);
    }
    let said = match with {
        [only] if count >= 2 => vec![text(&format!("{count} messages with ")), Part::With(*only)],
        [_] => [vec![text("Messaged ")], slots(with)].concat(),
        many if many.len() <= 3 => [vec![text("Messaged ")], slots(many)].concat(),
        many => vec![
            text("Messaged "),
            Part::With(many[0]),
            text(", "),
            Part::With(many[1]),
            text(&format!(" and {} more", many.len() - 2)),
        ],
    };
    if failed.is_empty() {
        said
    } else {
        missing([said, vec![text(". ")]].concat(), failed)
    }
}

/// The plain text of `parts`.
pub fn label(parts: &[Part], with: &[Who]) -> String {
    parts
        .iter()
        .map(|p| match p {
            Part::Text(t) => t.as_str(),
            Part::With(i) => with.get(*i).map(|w| w.name.as_str()).unwrap_or(""),
        })
        .collect()
}

/// The lines of one chat of `agent`: one for each turn of that conversation that sent a note
/// or a brief to another agent. `chat` is the conversation (a session id); `None`, or the
/// agent's default one, reads its default conversation. Oldest turn first.
pub fn lines(
    engine: &Engine,
    project: &str,
    agent: &str,
    chat: Option<&str>,
) -> Result<Vec<Line>, OrchError> {
    let names = known(&engine.store, project)?;
    let default = names.get(agent).and_then(|k| k.session.as_deref());
    let lane = chat.filter(|c| Some(*c) != default);
    group(
        said(&engine.store, project)?
            .into_iter()
            .filter(|s| s.initiator == agent && s.lane.as_deref() == lane),
        &names,
    )
}

fn group(
    said: impl Iterator<Item = Said>,
    names: &HashMap<String, Known>,
) -> Result<Vec<Line>, OrchError> {
    struct Turn {
        id: String,
        asked: Vec<String>,
        failed: Vec<String>,
        count: usize,
        last_ms: u64,
        first_ms: u64,
    }
    let mut turns: Vec<Turn> = Vec::new();
    for s in said {
        let at = match turns.iter().position(|t| t.id == s.turn) {
            Some(at) => at,
            None => {
                turns.push(Turn {
                    id: s.turn.clone(),
                    asked: Vec::new(),
                    failed: Vec::new(),
                    count: 0,
                    last_ms: 0,
                    first_ms: s.at_ms,
                });
                turns.len() - 1
            }
        };
        let t = &mut turns[at];
        if !t.asked.contains(&s.other) {
            t.asked.push(s.other.clone());
        }
        if s.failed && !t.failed.contains(&s.other) {
            t.failed.push(s.other.clone());
        }
        t.count += 1;
        t.last_ms = t.last_ms.max(s.at_ms);
    }
    turns.sort_by_key(|t| t.first_ms);
    Ok(turns
        .into_iter()
        .map(|t| {
            let failed = !t.failed.is_empty();
            let mut with: Vec<Who> = t.asked.iter().map(|a| who(names, a)).collect();
            for w in &mut with {
                w.failed = t.failed.contains(&w.agent);
            }
            let slots: Vec<usize> = (0..with.len()).collect();
            let missing: Vec<usize> = slots.iter().copied().filter(|i| with[*i].failed).collect();
            let parts = words(t.count, &slots, &missing);
            Line {
                label: label(&parts, &with),
                turn: t.id,
                with,
                count: t.count,
                last_ms: t.last_ms,
                failed,
                parts,
            }
        })
        .collect())
}

/// The messages between `a` and `b`, either way, oldest first (the newest [`MAX_MESSAGES`]).
pub fn talk(engine: &Engine, project: &str, a: &str, b: &str) -> Result<Talk, OrchError> {
    let names = known(&engine.store, project)?;
    for agent in [a, b] {
        if !names.contains_key(agent) {
            return Err(OrchError::UnknownAgent(agent.into()));
        }
    }
    let mut messages: Vec<Message> = said(&engine.store, project)?
        .into_iter()
        .filter(|s| (s.initiator == a && s.other == b) || (s.initiator == b && s.other == a))
        .map(|s| Message {
            author: s.author,
            text: s.text.chars().take(MAX_CHARS).collect(),
            at_ms: s.at_ms,
            work: s.work,
            failed: s.failed,
            pending: s.pending,
        })
        .collect();
    let from = messages.len().saturating_sub(MAX_MESSAGES);
    messages.drain(..from);
    Ok(Talk {
        with: [who(&names, a), who(&names, b)],
        messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> HashMap<String, Known> {
        let one = |title: &str, look: &str| Known {
            title: title.into(),
            look: look.into(),
            session: Some("s-ceo".into()),
        };
        HashMap::from([
            ("cmo".to_string(), one("CMO", "arch")),
            ("cso".to_string(), one("", "")),
            ("coo".to_string(), one("COO", "")),
            ("cfo".to_string(), one("CFO", "")),
        ])
    }

    fn say(turn: &str, other: &str, at_ms: u64, failed: bool) -> Said {
        Said {
            initiator: "ceo".into(),
            other: other.into(),
            author: "ceo".into(),
            text: "hi".into(),
            at_ms,
            turn: turn.into(),
            lane: None,
            work: None,
            failed,
            pending: false,
        }
    }

    fn one(items: Vec<Said>) -> Vec<Line> {
        group(items.into_iter(), &names()).unwrap()
    }

    #[test]
    fn one_agent_two_messages_one_agent_one_message_and_several_agents() {
        let l = one(vec![say("t1", "cmo", 1, false), say("t1", "cmo", 2, false)]);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].label, "2 messages with CMO");
        assert_eq!(l[0].count, 2);
        assert_eq!(l[0].last_ms, 2);
        assert_eq!(
            l[0].parts,
            vec![Part::Text("2 messages with ".into()), Part::With(0)]
        );
        assert_eq!(l[0].with[0].look, "arch");
        let l = one(vec![say("t1", "cmo", 1, false)]);
        assert_eq!(l[0].label, "Messaged CMO");
        let l = one(vec![say("t1", "cso", 1, false), say("t1", "cmo", 2, false)]);
        assert_eq!(
            l[0].label, "Messaged cso and CMO",
            "an untitled agent shows its name"
        );
        let l = one(vec![
            say("t1", "cso", 1, false),
            say("t1", "cmo", 2, false),
            say("t1", "coo", 3, false),
        ]);
        assert_eq!(l[0].label, "Messaged cso, CMO and COO");
        let l = one(vec![
            say("t1", "cso", 1, false),
            say("t1", "cmo", 2, false),
            say("t1", "coo", 3, false),
            say("t1", "cfo", 4, false),
        ]);
        assert_eq!(l[0].label, "Messaged cso, CMO and 2 more");
        assert_eq!(l[0].with.len(), 4);
    }

    #[test]
    fn each_turn_has_its_own_line_oldest_first_and_a_failed_reply_says_so() {
        let l = one(vec![
            say("t2", "cmo", 20, false),
            say("t1", "cso", 10, false),
            say("t2", "cmo", 21, true),
        ]);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].turn, "t1");
        assert_eq!(l[1].turn, "t2");
        assert!(l[1].failed);
        assert_eq!(l[1].label, "Reply from CMO did not arrive");
        assert_eq!(l[1].with.len(), 1);
        assert!(l[1].with[0].failed);
        assert!(!l[0].failed);
    }

    #[test]
    fn a_failed_reply_among_several_asked_keeps_every_agent_in_the_line() {
        let l = one(vec![
            say("t1", "cmo", 1, false),
            say("t1", "cso", 2, false),
            say("t1", "cso", 3, true),
        ]);
        assert_eq!(
            l[0].with
                .iter()
                .map(|w| w.agent.as_str())
                .collect::<Vec<_>>(),
            ["cmo", "cso"]
        );
        assert_eq!(
            l[0].with.iter().map(|w| w.failed).collect::<Vec<_>>(),
            [false, true]
        );
        assert!(l[0].failed);
        assert_eq!(
            l[0].label,
            "Messaged CMO and cso. Reply from cso did not arrive"
        );
        let l = one(vec![say("t1", "cmo", 1, true), say("t1", "cso", 2, true)]);
        assert_eq!(l[0].label, "Reply from CMO and cso did not arrive");
        let l = one(vec![
            say("t1", "cmo", 1, true),
            say("t1", "cmo", 2, false),
            say("t1", "cso", 3, true),
            say("t1", "coo", 4, false),
        ]);
        assert_eq!(
            l[0].label,
            "Messaged CMO, cso and COO. Reply from CMO and cso did not arrive"
        );
    }

    #[test]
    fn the_line_has_no_runtime_words() {
        let l = one(vec![say("t1", "cmo", 1, true), say("t2", "cmo", 2, false)]);
        for line in l {
            let t = line.label.to_lowercase();
            for word in ["task", "tool", "agents.", "delegate", "session", "start"] {
                assert!(!t.contains(word), "{t}");
            }
        }
    }
}
