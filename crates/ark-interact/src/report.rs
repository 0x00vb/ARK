//! Structured command results. A command builds a [`Report`]; each front-end renders it. The
//! terminal prints aligned rows and dim notes, the GUI lays out fields and tables, an API
//! serializes it. Nothing here knows about ANSI codes, widths, HTML or widgets.
//!
//! ```
//! use ark_interact::report::{Report, Table, Tone};
//!
//! let r = Report::titled("Usage · this session")
//!     .field("turns", "12")
//!     .field("tokens", "48k (46k in / 2.1k out)")
//!     .note("~ some numbers are estimates");
//! assert!(r.plain_text().contains("turns"));
//! ```

use serde::{Deserialize, Serialize};

/// How a piece of text should read. Front-ends map tones to their own styling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Normal,
    /// Secondary information.
    Muted,
    /// The thing to look at (a current item, a name).
    Accent,
    Success,
    Warning,
    Error,
}

impl Tone {
    fn is_normal(&self) -> bool {
        *self == Tone::Normal
    }
}

/// A labelled value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub label: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Tone::is_normal")]
    pub tone: Tone,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Column {
    /// Empty titles mean the table has no header row.
    pub title: String,
    #[serde(default)]
    pub align: Align,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub text: String,
    #[serde(default, skip_serializing_if = "Tone::is_normal")]
    pub tone: Tone,
}

impl Cell {
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Cell {
            text: text.into(),
            tone,
        }
    }
}

impl From<&str> for Cell {
    fn from(s: &str) -> Self {
        Cell::new(s, Tone::Normal)
    }
}
impl From<String> for Cell {
    fn from(s: String) -> Self {
        Cell::new(s, Tone::Normal)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// A tone for the whole row (a current item, an empty one).
    #[serde(default, skip_serializing_if = "Tone::is_normal")]
    pub tone: Tone,
    /// A command line that acts on this row (`/resume 8f3a`). A GUI makes the row clickable;
    /// a terminal ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
}

impl Table {
    /// Columns without a header row.
    pub fn plain(columns: usize) -> Self {
        Table {
            columns: (0..columns)
                .map(|_| Column {
                    title: String::new(),
                    align: Align::Left,
                })
                .collect(),
            rows: Vec::new(),
        }
    }
    pub fn with_header(titles: &[&str]) -> Self {
        Table {
            columns: titles
                .iter()
                .map(|t| Column {
                    title: t.to_string(),
                    align: Align::Left,
                })
                .collect(),
            rows: Vec::new(),
        }
    }
    /// Right-align column `i` (numbers).
    pub fn right(mut self, i: usize) -> Self {
        if let Some(c) = self.columns.get_mut(i) {
            c.align = Align::Right;
        }
        self
    }
    pub fn row<C: Into<Cell>>(mut self, cells: impl IntoIterator<Item = C>) -> Self {
        self.push(Row {
            cells: cells.into_iter().map(Into::into).collect(),
            tone: Tone::Normal,
            command: None,
        });
        self
    }
    pub fn push(&mut self, row: Row) {
        self.rows.push(row);
    }
    pub fn has_header(&self) -> bool {
        self.columns.iter().any(|c| !c.title.is_empty())
    }
}

/// Something a person can do next, as a command line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub label: String,
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Heading {
        text: String,
    },
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Tone::is_normal")]
        tone: Tone,
    },
    Fields {
        fields: Vec<Field>,
    },
    Table(Table),
    /// Preformatted text (JSON, a command's output).
    Code {
        text: String,
    },
    /// A quantity against a limit (tokens against the budget).
    Meter {
        label: String,
        used: u64,
        total: u64,
    },
    Actions {
        actions: Vec<Action>,
    },
}

/// A typed payload next to the blocks, for front-ends that have a dedicated view for it and
/// for API clients. `view` names the shape (`"context"`, `"usage"`, `"trace"`, ...).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Data {
    pub view: String,
    pub value: serde_json::Value,
}

/// What a command returns: a title, ordered blocks, and optional typed data.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub blocks: Vec<Block>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Data>,
}

impl Report {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn titled(title: impl Into<String>) -> Self {
        Report {
            title: Some(title.into()),
            ..Default::default()
        }
    }
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.blocks.is_empty()
    }
    pub fn heading(self, text: impl Into<String>) -> Self {
        self.block(Block::Heading { text: text.into() })
    }
    pub fn text(self, text: impl Into<String>) -> Self {
        self.say(Tone::Normal, text)
    }
    /// Secondary text.
    pub fn note(self, text: impl Into<String>) -> Self {
        self.say(Tone::Muted, text)
    }
    pub fn success(self, text: impl Into<String>) -> Self {
        self.say(Tone::Success, text)
    }
    pub fn warning(self, text: impl Into<String>) -> Self {
        self.say(Tone::Warning, text)
    }
    pub fn error(self, text: impl Into<String>) -> Self {
        self.say(Tone::Error, text)
    }
    pub fn say(self, tone: Tone, text: impl Into<String>) -> Self {
        self.block(Block::Text {
            text: text.into(),
            tone,
        })
    }
    /// A labelled value. Consecutive fields form one aligned group.
    pub fn field(self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.field_toned(label, value, Tone::Normal)
    }
    pub fn field_toned(
        mut self,
        label: impl Into<String>,
        value: impl Into<String>,
        tone: Tone,
    ) -> Self {
        let f = Field {
            label: label.into(),
            value: value.into(),
            tone,
        };
        match self.blocks.last_mut() {
            Some(Block::Fields { fields }) => fields.push(f),
            _ => self.blocks.push(Block::Fields { fields: vec![f] }),
        }
        self
    }
    pub fn table(self, t: Table) -> Self {
        self.block(Block::Table(t))
    }
    pub fn code(self, text: impl Into<String>) -> Self {
        self.block(Block::Code { text: text.into() })
    }
    pub fn meter(self, label: impl Into<String>, used: u64, total: u64) -> Self {
        self.block(Block::Meter {
            label: label.into(),
            used,
            total,
        })
    }
    /// Something to do next. Consecutive actions form one group.
    pub fn action(mut self, label: impl Into<String>, command: impl Into<String>) -> Self {
        let a = Action {
            label: label.into(),
            command: command.into(),
        };
        match self.blocks.last_mut() {
            Some(Block::Actions { actions }) => actions.push(a),
            _ => self.blocks.push(Block::Actions { actions: vec![a] }),
        }
        self
    }
    pub fn block(mut self, b: Block) -> Self {
        self.blocks.push(b);
        self
    }
    pub fn blocks(mut self, b: impl IntoIterator<Item = Block>) -> Self {
        self.blocks.extend(b);
        self
    }
    /// Attach a typed payload (see [`Data`]).
    pub fn data(mut self, view: impl Into<String>, value: impl Serialize) -> Self {
        self.data = Some(Data {
            view: view.into(),
            value: serde_json::to_value(value).unwrap_or_default(),
        });
        self
    }

    /// A plain-text rendering without styling: for logs, tests and API clients that want text.
    pub fn plain_text(&self) -> String {
        let mut out = Vec::new();
        if let Some(t) = &self.title {
            out.push(t.clone());
        }
        for b in &self.blocks {
            match b {
                Block::Heading { text } => {
                    out.push(String::new());
                    out.push(text.clone());
                }
                Block::Text { text, .. } => out.extend(text.lines().map(|l| format!("  {l}"))),
                Block::Fields { fields } => {
                    let w = fields
                        .iter()
                        .map(|f| f.label.chars().count())
                        .max()
                        .unwrap_or(0);
                    for f in fields {
                        out.push(format!("  {:<w$}  {}", f.label, f.value));
                    }
                }
                Block::Table(t) => {
                    for l in table_lines(t) {
                        out.push(format!("  {l}"));
                    }
                }
                Block::Code { text } => out.extend(text.lines().map(|l| format!("    {l}"))),
                Block::Meter { label, used, total } => {
                    out.push(format!("  {label}: {used} / {total}"))
                }
                Block::Actions { actions } => {
                    for a in actions {
                        out.push(format!("  {}  {}", a.command, a.label));
                    }
                }
            }
        }
        out.join("\n")
    }
}

/// Column widths for a table, measured in characters.
pub fn column_widths(t: &Table) -> Vec<usize> {
    let mut w: Vec<usize> = t.columns.iter().map(|c| c.title.chars().count()).collect();
    for r in &t.rows {
        for (i, c) in r.cells.iter().enumerate() {
            if i < w.len() {
                w[i] = w[i].max(c.text.chars().count());
            }
        }
    }
    w
}

fn table_lines(t: &Table) -> Vec<String> {
    let w = column_widths(t);
    let fmt = |cells: Vec<&str>| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let width = w.get(i).copied().unwrap_or(0);
                match t.columns.get(i).map(|c| c.align) {
                    Some(Align::Right) => format!("{c:>width$}"),
                    _ => format!("{c:<width$}"),
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = Vec::new();
    if t.has_header() {
        out.push(fmt(t.columns.iter().map(|c| c.title.as_str()).collect()));
    }
    for r in &t.rows {
        out.push(fmt(r.cells.iter().map(|c| c.text.as_str()).collect()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consecutive_fields_and_actions_group() {
        let r = Report::titled("T")
            .field("a", "1")
            .field("bb", "2")
            .note("n")
            .field("c", "3")
            .action("switch", "/resume x")
            .action("new", "/new");
        assert_eq!(r.blocks.len(), 4);
        let text = r.plain_text();
        assert!(text.contains("  a   1\n  bb  2"), "{text}");
        assert!(text.contains("/resume x  switch"));
    }

    #[test]
    fn tables_align_and_serialize() {
        let t = Table::with_header(&["Source", "Tokens"])
            .right(1)
            .row(["Instructions", "412"])
            .row(["Task", "9"]);
        let r = Report::new()
            .table(t)
            .data("context", serde_json::json!({ "total": 421 }));
        let text = r.plain_text();
        assert!(text.contains("Source        Tokens"), "{text}");
        assert!(text.contains("Task               9"), "{text}");
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["blocks"][0]["type"], "table");
        assert_eq!(json["data"]["view"], "context");
        let back: Report = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }
}
