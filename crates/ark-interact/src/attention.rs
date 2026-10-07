//! What waits for the person, in every project of a runtime: one list for both front-ends
//! (ADR 0043). It is a view over records ARK already keeps, for any kind of work:
//!
//! - a question a task keeps (ADR 0036). A chat's own question lives with its turn: the
//!   terminal asks it inline, a window shows it in that chat or in the tray;
//! - work the person started that failed (from a chat, a schedule, the board, a command);
//! - a result posted into a chat (ADR 0025), because nobody was there when it finished.
//!
//! The only thing stored here is a mark per item: cleared, when, and later by whom. A question
//! is never cleared; it leaves the list when it is answered.

use std::collections::{HashMap, HashSet};

use ark_orchestrate::{Origin, TaskRecord, TaskState};
use ark_sdk::prelude::{ApprovalRequest, Engine, ProjectId};
use serde::Serialize;
use serde_json::json;

use crate::approval::{approval_choices, approval_title, Choice};
use crate::fmt::clip;
use crate::labels::Labels;
use crate::report::{Cell, Report, Row, Table, Tone};

/// How long a failure or a result stays in the list unless it is cleared or opened.
pub const KEPT_MS: u64 = 7 * 24 * 3600 * 1000;

/// Record kind of a cleared mark: id = the item's id, scope = its project.
const CLEARED: &str = "attention_cleared";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// It waits for an answer.
    Question,
    /// Work the person started did not finish.
    Failed,
    /// Work finished while nobody was in its chat.
    Finished,
}

/// One thing that waits for the person.
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct Item {
    /// The runtime it waits on, when it is not this one: a link's name (`crate::links`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// The question's id for a question (a client answers it by this id); else
    /// `failed/<task>/<finished>` or `finished/<task>/<finished>`, so the same task failing
    /// again after a retry is a new item.
    pub id: String,
    pub kind: Kind,
    pub project: String,
    /// The chat a result was posted into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The agent whose work it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// What: the action asked about, or the work's objective.
    pub title: String,
    /// Where: the project, then the chat or the agent.
    pub place: String,
    /// The item as one sentence, as the terminal prints it.
    pub line: String,
    pub at_ms: u64,
    /// A question's answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Choice>,
}

impl Item {
    pub fn is_question(&self) -> bool {
        self.kind == Kind::Question
    }

    fn tone(&self) -> Tone {
        match self.kind {
            Kind::Question => Tone::Warning,
            Kind::Failed => Tone::Error,
            Kind::Finished => Tone::Success,
        }
    }
}

/// Names of projects and chats, read once per list.
struct Names<'a> {
    engine: &'a Engine,
    projects: HashMap<String, Option<String>>,
    chats: HashMap<String, String>,
}

impl<'a> Names<'a> {
    fn new(engine: &'a Engine) -> Self {
        Names {
            engine,
            projects: HashMap::new(),
            chats: HashMap::new(),
        }
    }

    /// The project's name; none when the project is gone.
    fn project(&mut self, id: &str) -> Option<String> {
        let engine = self.engine;
        self.projects
            .entry(id.to_string())
            .or_insert_with(|| {
                engine
                    .store
                    .get_project(&ProjectId(id.to_string()))
                    .ok()
                    .flatten()
                    .map(|p| p.name)
            })
            .clone()
    }

    fn chat(&mut self, id: &str) -> String {
        let engine = self.engine;
        self.chats
            .entry(id.to_string())
            .or_insert_with(|| {
                engine
                    .store
                    .get_session(id)
                    .ok()
                    .flatten()
                    .and_then(|s| s["title"].as_str().map(str::to_string))
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| "Untitled".into())
            })
            .clone()
    }

    fn place(&mut self, project: &str, chat: Option<&str>, agent: Option<&str>) -> Option<String> {
        let p = self.project(project)?;
        Some(match (chat, agent) {
            (Some(c), _) => format!("{p} · {}", clip(&self.chat(c), 40)),
            (None, Some(a)) => format!("{p} · {a}"),
            (None, None) => p,
        })
    }
}

/// What waits here and on the linked runtimes (ADR 0043 step 4): this runtime's list and each
/// link's as last read, questions first, then the newest first. Never waits on the network.
pub fn everywhere(engine: &Engine, labels: &Labels, links: &crate::links::Links) -> Vec<Item> {
    let mut items = list(engine, labels);
    items.extend(links.items());
    items.sort_by(|a, b| {
        b.is_question()
            .cmp(&a.is_question())
            .then(b.at_ms.cmp(&a.at_ms))
    });
    items
}

/// Links that had items and stopped answering: their items stay listed, as last read, and
/// cannot be answered until the link answers again.
pub fn away(links: &crate::links::Links) -> Vec<String> {
    links
        .shown()
        .into_iter()
        .filter(|l| l.state == crate::links::State::Down && l.had_items)
        .map(|l| l.name)
        .collect()
}

/// What a client says about links that are not well, beside the list: one that refused this
/// computer or is too old, and with `away`, one that had items and stopped answering (a
/// window shows that on the items themselves). Nothing while all is well.
pub fn link_notes(links: &crate::links::Links, away: bool) -> Vec<String> {
    use crate::links::State;
    links
        .shown()
        .into_iter()
        .filter_map(|l| match l.state {
            State::Refused => Some(format!("{} signed this computer out: pair again.", l.name)),
            State::Down if away && l.had_items => Some(format!(
                "{} is not answering; its items may be out of date.",
                l.name
            )),
            State::TooOld => Some(format!(
                "{} runs an older version: what waits there is not shown here.",
                l.name
            )),
            _ => None,
        })
        .collect()
}

/// Everything that waits for the person now: questions first, then the newest first.
pub fn list(engine: &Engine, labels: &Labels) -> Vec<Item> {
    list_at(engine, labels, now_ms())
}

fn list_at(engine: &Engine, labels: &Labels, now: u64) -> Vec<Item> {
    // Only tasks that can make an item are read in full: an open question, a finish in the
    // window, or a schedule's runs (its latest run decides).
    let tasks = ark_orchestrate::tasks_where(&engine.store, |v| {
        !v["question"].is_null()
            || !v["schedule"].is_null()
            || v["finished_ms"]
                .as_u64()
                .is_some_and(|f| now.saturating_sub(f) <= KEPT_MS)
    });
    let cleared = cleared(engine, now);
    let mut names = Names::new(engine);
    let mut items = Vec::new();

    for t in &tasks {
        let Some(q) = t.question.as_ref().filter(|q| q.open()) else {
            continue;
        };
        let Some(place) = names.place(&t.project, None, Some(&t.owner)) else {
            continue;
        };
        items.push(question(
            labels,
            q.id.clone(),
            &q.request,
            t.project.clone(),
            None,
            Some(t.id.clone()),
            place,
            q.asked_ms,
        ));
    }

    // A schedule's failure stays only while it is that schedule's latest run.
    let mut latest: HashMap<&str, u64> = HashMap::new();
    for t in &tasks {
        if let (Some(s), Some(f), true) = (&t.schedule, t.finished_ms, t.state.is_final()) {
            let e = latest.entry(s.as_str()).or_insert(0);
            *e = (*e).max(f);
        }
    }
    let mut posted = Posted::new(engine);
    for t in &tasks {
        let Some(finished) = t.finished_ms.filter(|f| now.saturating_sub(*f) <= KEPT_MS) else {
            continue;
        };
        let kind = match t.state {
            TaskState::Failed if person_started(t) => Kind::Failed,
            TaskState::Done if t.report_to.is_some() => Kind::Finished,
            _ => continue,
        };
        if let Some(s) = &t.schedule {
            if latest.get(s.as_str()) != Some(&finished) {
                continue;
            }
        }
        // A result a waiting chat turn took was seen there, not posted.
        if let Some(chat) = &t.report_to {
            if !posted.has(chat, &t.id) {
                continue;
            }
        }
        let id = format!(
            "{}/{}/{finished}",
            if kind == Kind::Failed {
                "failed"
            } else {
                "finished"
            },
            t.id
        );
        if cleared.contains(&id) {
            continue;
        }
        let Some(place) = names.place(&t.project, t.report_to.as_deref(), Some(&t.owner)) else {
            continue;
        };
        let title = clip(&t.objective, 80);
        let line = match kind {
            Kind::Failed => format!("Did not finish: {title}"),
            _ => format!("Done: {title}"),
        };
        items.push(Item {
            runtime: None,
            id,
            kind,
            project: t.project.clone(),
            chat: t.report_to.clone(),
            task: Some(t.id.clone()),
            agent: Some(t.owner.clone()),
            title,
            place,
            line,
            at_ms: finished,
            choices: Vec::new(),
        });
    }
    items.sort_by(|a, b| {
        b.is_question()
            .cmp(&a.is_question())
            .then(b.at_ms.cmp(&a.at_ms))
    });
    items
}

#[allow(clippy::too_many_arguments)]
fn question(
    labels: &Labels,
    id: String,
    request: &ApprovalRequest,
    project: String,
    chat: Option<String>,
    task: Option<String>,
    place: String,
    at_ms: u64,
) -> Item {
    let title = labels.approval_line(request);
    Item {
        runtime: None,
        line: format!("{} {title}", approval_title(request)),
        id,
        kind: Kind::Question,
        project,
        chat,
        task,
        agent: request.agent.clone(),
        title,
        place,
        at_ms,
        choices: approval_choices(request),
    }
}

/// Started by the person (a chat, a schedule, the board, a command), not delegated by an
/// agent: a delegated task's failure goes to the agent that delegated it.
fn person_started(t: &TaskRecord) -> bool {
    matches!(t.origin, Origin::External) && t.parent.is_none()
}

/// Which tasks each chat has a post about, read once per chat.
struct Posted<'a> {
    engine: &'a Engine,
    chats: HashMap<String, HashSet<String>>,
}

impl<'a> Posted<'a> {
    fn new(engine: &'a Engine) -> Self {
        Posted {
            engine,
            chats: HashMap::new(),
        }
    }

    fn has(&mut self, chat: &str, task: &str) -> bool {
        let engine = self.engine;
        self.chats
            .entry(chat.to_string())
            .or_insert_with(|| {
                let mut works = HashSet::new();
                let mut after = 0;
                loop {
                    let page =
                        ark_sdk::prelude::ark_runtime::session::posts(engine, chat, after, 500)
                            .unwrap_or_default();
                    works.extend(page.iter().filter_map(|(_, p)| p.work.clone()));
                    match page.last() {
                        Some((seq, _)) if page.len() == 500 => after = *seq,
                        _ => break,
                    }
                }
                works
            })
            .contains(task)
    }
}

/// Ids of cleared items. Marks older than anything they could still hide are dropped.
fn cleared(engine: &Engine, now: u64) -> HashSet<String> {
    let mut ids = HashSet::new();
    for v in engine.store.list_records(CLEARED, None).unwrap_or_default() {
        let Some(id) = v["id"].as_str() else { continue };
        let at = v["at_ms"].as_u64().unwrap_or(0);
        if now.saturating_sub(at) > 2 * KEPT_MS {
            let _ = engine.store.delete_record(CLEARED, id);
        } else {
            ids.insert(id.to_string());
        }
    }
    ids
}

/// Clear an item: it leaves the list in every client and after a restart. A question cannot be
/// cleared (it is answered). The work stays where it was: its chat, Agents, the board.
pub fn clear(engine: &Engine, item: &Item) -> anyhow::Result<()> {
    anyhow::ensure!(!item.is_question(), "a question leaves when it is answered");
    engine.store.put_record(
        CLEARED,
        &item.id,
        &item.project,
        // `by` names the person who cleared it once a runtime has several (ADR 0043).
        &json!({ "id": item.id, "at_ms": now_ms(), "by": null }),
    )?;
    Ok(())
}

/// The person opened `chat`: what was posted there is seen. Returns how many items cleared.
pub fn seen_chat(engine: &Engine, chat: &str) -> usize {
    let items = list(engine, &Labels::default());
    items
        .iter()
        .filter(|i| !i.is_question() && i.chat.as_deref() == Some(chat))
        .filter(|i| clear(engine, i).is_ok())
        .count()
}

/// The record kind of an item a notification went out for (ADR 0047): kept so a restart or a
/// second process does not send it again.
pub const NOTIFIED: &str = "attention_notified";

/// What a notification about an item says, the same on every device and in a native window's
/// own notification.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Notice {
    /// The item: a tap opens it (`#item=<id>` on the page).
    pub id: String,
    /// Where: the project, then the chat or the agent.
    pub title: String,
    /// What, as one sentence.
    pub body: String,
    /// Notices with one tag replace each other: a question has its own; a project's results and
    /// failures share one, so a schedule that posts every run does not stack them.
    pub tag: String,
    /// It waits for an answer: delivered first.
    pub urgent: bool,
    pub at_ms: u64,
}

/// The notice for an item.
pub fn notice(item: &Item) -> Notice {
    Notice {
        id: item.id.clone(),
        title: item.place.clone(),
        body: clip(&item.line, 140),
        tag: match item.kind {
            Kind::Question => format!("question/{}", item.id),
            _ => format!("work/{}", item.project),
        },
        urgent: item.is_question(),
        at_ms: item.at_ms,
    }
}

/// The ids notices went out for. Marks older than any item could be are dropped.
pub fn notified(engine: &Engine) -> HashSet<String> {
    let now = now_ms();
    let mut ids = HashSet::new();
    for v in engine
        .store
        .list_records(NOTIFIED, None)
        .unwrap_or_default()
    {
        let Some(id) = v["id"].as_str() else { continue };
        if now.saturating_sub(v["at_ms"].as_u64().unwrap_or(0)) > 2 * KEPT_MS {
            let _ = engine.store.delete_record(NOTIFIED, id);
        } else {
            ids.insert(id.to_string());
        }
    }
    ids
}

/// A notice went out for `id`.
pub fn mark_notified(engine: &Engine, id: &str) -> anyhow::Result<()> {
    engine
        .store
        .put_record(NOTIFIED, id, "", &json!({ "id": id, "at_ms": now_ms() }))?;
    Ok(())
}

/// The list as the terminal prints it. `/waiting` numbers the rows.
pub fn report(items: &[Item]) -> Report {
    report_with(items, &[])
}

/// [`report`], with what is wrong with links (`link_notes`), one muted line each.
pub fn report_with(items: &[Item], notes: &[String]) -> Report {
    if items.is_empty() {
        let mut r = Report::new().note("Nothing is waiting for you.");
        for n in notes {
            r = r.note(n.clone());
        }
        return r;
    }
    let mut t = Table::plain(3).right(0);
    for (i, it) in items.iter().enumerate() {
        t.push(Row {
            cells: vec![
                Cell::new(format!("{}", i + 1), Tone::Muted),
                Cell::new(it.line.clone(), it.tone()),
                Cell::new(it.place.clone(), Tone::Muted),
            ],
            tone: Tone::Normal,
            command: Some(format!("/waiting {}", i + 1)),
        });
    }
    let mut r = Report::titled("Waiting for you").table(t);
    if items.iter().any(Item::is_question) {
        r = r.note("Answer a question: /waiting <n> once|session|always|deny");
    }
    r = r.note("Open one: /waiting <n>. Clear what is done or failed: /waiting clear");
    for n in notes {
        r = r.note(n.clone());
    }
    r
}

/// The terminal's line when what waits changes: `2 waiting for you · /waiting`.
pub fn nudge(n: usize) -> Option<String> {
    (n > 0).then(|| format!("{n} waiting for you · /waiting"))
}

/// One item opened: a question with its answers, or the work's result.
pub fn opened(engine: &Engine, item: &Item, n: usize) -> Report {
    let mut r = Report::titled(item.line.clone()).field("Where", item.place.clone());
    if item.is_question() {
        let answers: Vec<String> = item.choices.iter().map(|c| c.label.clone()).collect();
        return r.note(format!(
            "{}. Answer: /waiting {n} once|session|always|deny",
            answers.join(", ")
        ));
    }
    if let Some(t) = text(engine, item) {
        r = match item.kind {
            Kind::Failed => r.error(t),
            _ => r.text(t),
        };
    }
    if let Some(chat) = &item.chat {
        r = r.note(format!("Its chat: /resume {}", short(chat)));
    }
    r
}

/// What the work said: its result, or why it failed.
pub fn text(engine: &Engine, item: &Item) -> Option<String> {
    let id = item.task.as_deref()?;
    let t = ark_orchestrate::all_tasks(&engine.store)
        .into_iter()
        .find(|t| t.id == id)?;
    match item.kind {
        Kind::Failed => Some(t.error.unwrap_or_else(|| "It stopped.".into())),
        Kind::Finished => t.result,
        Kind::Question => None,
    }
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
