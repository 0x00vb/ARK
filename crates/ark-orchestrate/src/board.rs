//! The project's board: the work the project needs, who has each piece and how far it is. One
//! board per project, kept across chats, agent runs, restarts and team changes. The person and
//! the agents change the same [`WorkItem`]s; a front-end draws them (columns, a list, a report).
//!
//! An item is an outcome ("Draft chapter 3"), not a run. A task is one agent's run toward it:
//! handing an item to an agent creates a task linked to it, and the orchestrator keeps the item
//! up to date from its tasks, so agents spend no calls on bookkeeping.
//!
//! | Rule | Why |
//! |---|---|
//! | A task linked to an item moves it to `doing` with that agent as owner | the board shows who works on what without the agent saying so |
//! | An item an agent works on (an open task) cannot be handed to another agent | two agents never do the same work |
//! | A linked task that fails marks the item blocked with the error | the failure is seen where the work is tracked |
//! | The last open linked task finishing moves the item to `done` | done means done; the planner may move it back |
//! | A worker's brief carries its item: title, note, the last steps | an agent continues work another agent or session left |
//! | The log keeps the last [`LOG_KEPT`] steps | history is a summary, not a transcript |

use serde::{Deserialize, Serialize};

use crate::brief::clip;
use crate::record::now_ms;

/// The record kind items are stored under, per project.
pub(crate) const WORK: &str = "orch_work";

/// The owner that stands for the person.
pub const PERSON: &str = "user";

/// Steps kept in an item's log.
pub const LOG_KEPT: usize = 30;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    #[default]
    Todo,
    Doing,
    Done,
}

impl WorkState {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkState::Todo => "todo",
            WorkState::Doing => "doing",
            WorkState::Done => "done",
        }
    }
    /// The column's name.
    pub fn label(self) -> &'static str {
        match self {
            WorkState::Todo => "To do",
            WorkState::Doing => "In progress",
            WorkState::Done => "Done",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().replace(['-', '_'], " ").as_str() {
            "todo" | "to do" => Some(WorkState::Todo),
            "doing" | "in progress" | "progress" => Some(WorkState::Doing),
            "done" => Some(WorkState::Done),
            _ => None,
        }
    }
    pub const ALL: [WorkState; 3] = [WorkState::Todo, WorkState::Doing, WorkState::Done];
}

/// One step in an item's history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkStep {
    pub at_ms: u64,
    /// An agent's name, [`PERSON`], or `chat` for the person's conversation.
    pub by: String,
    pub text: String,
}

/// A piece of the project's work. See the module docs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkItem {
    /// `w-` and eight hex digits.
    pub id: String,
    pub project: String,
    pub title: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub state: WorkState,
    /// An agent's name, [`PERSON`], or nobody.
    #[serde(default)]
    pub owner: Option<String>,
    /// What it waits for, in words, while it cannot go on.
    #[serde(default)]
    pub blocked: Option<String>,
    /// Tasks run for it, oldest first.
    #[serde(default)]
    pub tasks: Vec<String>,
    /// Its place in its column (lower first).
    #[serde(default)]
    pub rank: f64,
    #[serde(default)]
    pub log: Vec<WorkStep>,
    /// Who added it.
    #[serde(default)]
    pub by: String,
    pub created_ms: u64,
    #[serde(default)]
    pub updated_ms: u64,
    #[serde(default)]
    pub done_ms: Option<u64>,
}

impl WorkItem {
    pub(crate) fn new(project: &str, by: &str, title: &str) -> Self {
        let now = now_ms();
        let mut w = WorkItem {
            id: format!("w-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
            project: project.to_string(),
            title: clip(title, 200),
            note: String::new(),
            state: WorkState::Todo,
            owner: None,
            blocked: None,
            tasks: Vec::new(),
            rank: 0.0,
            log: Vec::new(),
            by: by.to_string(),
            created_ms: now,
            updated_ms: now,
            done_ms: None,
        };
        w.step(by, "added it");
        w
    }

    /// Waiting for the person: blocked and theirs.
    pub fn needs_person(&self) -> bool {
        self.state != WorkState::Done
            && self.blocked.is_some()
            && self.owner.as_deref() == Some(PERSON)
    }

    pub(crate) fn step(&mut self, by: &str, text: impl Into<String>) {
        self.log.push(WorkStep {
            at_ms: now_ms(),
            by: by.to_string(),
            text: clip(&text.into(), 300),
        });
        let n = self.log.len();
        if n > LOG_KEPT {
            self.log.drain(..n - LOG_KEPT);
        }
        self.updated_ms = now_ms();
    }

    pub(crate) fn set_state(&mut self, by: &str, state: WorkState) {
        if self.state == state {
            return;
        }
        self.state = state;
        self.done_ms = (state == WorkState::Done).then(now_ms);
        if state == WorkState::Done {
            self.blocked = None;
        }
        self.step(by, format!("moved it to {}", state.label()));
    }

    pub(crate) fn set_owner(&mut self, by: &str, owner: Option<String>) {
        if self.owner == owner {
            return;
        }
        let text = match &owner {
            Some(o) if o == by => "took it".to_string(),
            Some(o) if o == PERSON => "gave it to the person".to_string(),
            Some(o) => format!("gave it to {o}"),
            None => "left it unassigned".to_string(),
        };
        self.owner = owner;
        self.step(by, text);
    }

    pub(crate) fn set_blocked(&mut self, by: &str, blocked: Option<String>) {
        let blocked = blocked.map(|b| clip(&b, 300)).filter(|b| !b.is_empty());
        if self.blocked == blocked {
            return;
        }
        let text = match &blocked {
            Some(b) => format!("blocked: {b}"),
            None => "unblocked".to_string(),
        };
        self.blocked = blocked;
        self.step(by, text);
    }

    /// One line for a list: `w-1a2b3c4d [doing, writer] Draft chapter 3 (blocked: …)`.
    pub fn line(&self) -> String {
        let mut s = format!("{} [{}", self.id, self.state.as_str());
        if let Some(o) = &self.owner {
            s.push_str(&format!(", {}", if o == PERSON { "the person" } else { o }));
        }
        s.push_str(&format!("] {}", self.title));
        if let Some(b) = &self.blocked {
            s.push_str(&format!(" (blocked: {})", clip(b, 120)));
        }
        s
    }
}

/// A change to an item; `None` leaves a field as it is.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkChange {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub state: Option<WorkState>,
    /// An agent, [`PERSON`], or empty for nobody.
    #[serde(default)]
    pub owner: Option<String>,
    /// What it waits for; empty clears it.
    #[serde(default)]
    pub blocked: Option<String>,
    /// Put it before this item in its column; with `state` and no `before`, at the end.
    #[serde(default)]
    pub before: Option<String>,
}

impl WorkChange {
    pub fn state(mut self, s: WorkState) -> Self {
        self.state = Some(s);
        self
    }
    pub fn owner(mut self, o: impl Into<String>) -> Self {
        self.owner = Some(o.into());
        self
    }
    pub fn blocked(mut self, b: impl Into<String>) -> Self {
        self.blocked = Some(b.into());
        self
    }
    pub fn note(mut self, n: impl Into<String>) -> Self {
        self.note = Some(n.into());
        self
    }
    pub fn title(mut self, t: impl Into<String>) -> Self {
        self.title = Some(t.into());
        self
    }
    pub fn before(mut self, id: impl Into<String>) -> Self {
        self.before = Some(id.into());
        self
    }
}

/// Items in board order: by column, then by place (Done: the latest first).
pub fn ordered(mut items: Vec<WorkItem>) -> Vec<WorkItem> {
    let col = |s: WorkState| WorkState::ALL.iter().position(|x| *x == s).unwrap_or(0);
    items.sort_by(|a, b| {
        col(a.state).cmp(&col(b.state)).then_with(|| {
            if a.state == WorkState::Done {
                b.done_ms.cmp(&a.done_ms)
            } else {
                a.rank
                    .partial_cmp(&b.rank)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.created_ms.cmp(&b.created_ms))
            }
        })
    });
    items
}

/// The rank that puts an item before `before` in `column` (sorted, without the item), or at
/// its end.
pub(crate) fn rank_before(column: &[&WorkItem], before: Option<&str>) -> f64 {
    let at = before.and_then(|b| column.iter().position(|w| w.id == b));
    match at {
        None => column.last().map_or(0.0, |w| w.rank + 1.0),
        Some(0) => column[0].rank - 1.0,
        Some(i) => (column[i - 1].rank + column[i].rank) / 2.0,
    }
}

/// The board in a few words, for a tool's description: `4 to do, 2 in progress (1 blocked), 3 done`.
pub fn summary(items: &[WorkItem]) -> String {
    if items.is_empty() {
        return "The board is empty.".into();
    }
    let n = |s: WorkState| items.iter().filter(|w| w.state == s).count();
    let blocked = items
        .iter()
        .filter(|w| w.state != WorkState::Done && w.blocked.is_some())
        .count();
    let mut s = format!(
        "Board: {} to do, {} in progress, {} done",
        n(WorkState::Todo),
        n(WorkState::Doing),
        n(WorkState::Done)
    );
    if blocked > 0 {
        s.push_str(&format!(", {blocked} blocked"));
    }
    s.push('.');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, state: WorkState, rank: f64) -> WorkItem {
        let mut w = WorkItem::new("p", PERSON, id);
        w.id = id.into();
        w.state = state;
        w.rank = rank;
        w
    }

    #[test]
    fn ranks_put_an_item_between_its_neighbours() {
        let a = item("a", WorkState::Todo, 0.0);
        let b = item("b", WorkState::Todo, 1.0);
        let col = vec![&a, &b];
        assert_eq!(rank_before(&col, None), 2.0);
        assert_eq!(rank_before(&col, Some("a")), -1.0);
        assert_eq!(rank_before(&col, Some("b")), 0.5);
        assert_eq!(rank_before(&[], Some("x")), 0.0);
    }

    #[test]
    fn the_board_is_ordered_by_column_then_place_and_done_latest_first() {
        let mut d1 = item("d1", WorkState::Done, 0.0);
        d1.done_ms = Some(1);
        let mut d2 = item("d2", WorkState::Done, 0.0);
        d2.done_ms = Some(2);
        let v = ordered(vec![
            d1,
            item("t2", WorkState::Todo, 2.0),
            item("g", WorkState::Doing, 0.0),
            d2,
            item("t1", WorkState::Todo, 1.0),
        ]);
        let ids: Vec<&str> = v.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["t1", "t2", "g", "d2", "d1"]);
    }

    #[test]
    fn changes_leave_a_short_history() {
        let mut w = WorkItem::new("p", "chat", "Draft chapter 3");
        w.set_owner("chat", Some("writer".into()));
        w.set_state("writer", WorkState::Doing);
        w.set_state("writer", WorkState::Doing);
        w.set_blocked("writer", Some("needs the outline".into()));
        w.set_owner("writer", Some(PERSON.into()));
        assert!(w.needs_person());
        w.set_state(PERSON, WorkState::Done);
        assert!(w.blocked.is_none() && w.done_ms.is_some() && !w.needs_person());
        let steps: Vec<&str> = w.log.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            steps,
            [
                "added it",
                "gave it to writer",
                "moved it to In progress",
                "blocked: needs the outline",
                "gave it to the person",
                "moved it to Done"
            ]
        );
        for _ in 0..LOG_KEPT {
            w.step(PERSON, "x");
        }
        assert_eq!(w.log.len(), LOG_KEPT);
        assert_eq!(WorkState::parse("In progress"), Some(WorkState::Doing));
        assert_eq!(WorkState::parse("to-do"), Some(WorkState::Todo));
    }
}
