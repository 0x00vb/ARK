//! The few shapes every format maps to: text, a workbook of sheets, a document of blocks. The
//! agent, the window and every format meet here, so a format only translates bytes to a shape
//! and back, and an edit means the same thing whichever file it lands in.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value as Json;

use crate::Error;

/// What a file is, as a person sees it: the key of its view in the window. Several kinds read
/// as the same shape ([`Content`]): Markdown is text, a PDF reads as a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Text,
    Markdown,
    Sheet,
    Document,
    Pdf,
    Image,
    Map,
    Model,
}

impl Kind {
    pub const ALL: [Kind; 8] = [
        Kind::Text,
        Kind::Markdown,
        Kind::Sheet,
        Kind::Document,
        Kind::Pdf,
        Kind::Image,
        Kind::Map,
        Kind::Model,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Markdown => "markdown",
            Kind::Sheet => "sheet",
            Kind::Document => "document",
            Kind::Pdf => "pdf",
            Kind::Image => "image",
            Kind::Map => "map",
            Kind::Model => "model",
        }
    }
    /// The word a person reads.
    pub fn noun(self) -> &'static str {
        match self {
            Kind::Text => "text file",
            Kind::Markdown => "Markdown document",
            Kind::Sheet => "spreadsheet",
            Kind::Document => "document",
            Kind::Pdf => "PDF",
            Kind::Image => "image",
            Kind::Map => "map",
            Kind::Model => "3D model",
        }
    }
    /// Whether it is plain text the window saves whole.
    pub fn is_text(self) -> bool {
        matches!(self, Kind::Text | Kind::Markdown | Kind::Map)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Text(String),
    Sheet(Workbook),
    Document(Document),
    /// A file the agent cannot read as text (an image): what it is, for the agent; the window
    /// shows the bytes.
    Media(Media),
}

impl Content {
    /// The kind a file with this content is when its format says nothing more.
    pub fn kind(&self) -> Kind {
        match self {
            Content::Text(_) => Kind::Text,
            Content::Sheet(_) => Kind::Sheet,
            Content::Document(_) => Kind::Document,
            Content::Media(_) => Kind::Image,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Media {
    /// `image/png`.
    pub media_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

// ---- sheets ------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    /// Charts embedded in worksheets, in workbook and drawing order. They are a view of the
    /// workbook, not an editable shape: XLSX keeps the chart parts byte for byte.
    pub charts: Vec<Chart>,
    /// A malformed chart must not make the cells disappear. The window shows these beside any
    /// charts it could read.
    pub chart_warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Chart {
    pub sheet: String,
    pub title: String,
    pub kind: ChartKind,
    pub series: Vec<ChartSeries>,
    pub sampled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum ChartKind {
    Bar,
    Column,
    Line,
    Area,
    Pie,
    Doughnut,
    Scatter,
    #[default]
    Unsupported,
}

impl ChartKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChartKind::Bar => "bar",
            ChartKind::Column => "column",
            ChartKind::Line => "line",
            ChartKind::Area => "area",
            ChartKind::Pie => "pie",
            ChartKind::Doughnut => "doughnut",
            ChartKind::Scatter => "scatter",
            ChartKind::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ChartSeries {
    pub name: String,
    pub categories: Vec<String>,
    pub values: Vec<f64>,
    pub x_values: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Sheet {
    pub name: String,
    /// By (row, column), both from 0.
    pub cells: BTreeMap<(u32, u32), Cell>,
    /// Which sheet of the file this one was read from; `None` for a new sheet.
    pub origin: Option<usize>,
    /// Rows of the file after the last one read. A read to show a file stops after
    /// [`crate::READ_CELLS`] cells of a sheet, since the agent and the window see only its start;
    /// a program reads the rest. 0 when the whole sheet was read (always for an edit).
    pub unread: u32,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Cell {
    pub value: Value,
    /// Without the leading `=`.
    pub formula: Option<String>,
    pub fmt: Fmt,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Value {
    #[default]
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    Error(String),
}

/// How a number shows. Formats read it from the file's styles; everything else is General.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Fmt {
    #[default]
    General,
    /// A date serial (days since 1899-12-30).
    Date,
    DateTime,
    Percent(u8),
    Decimals(u8),
    /// With thousands separators (`1,234.50`).
    Grouped(u8),
}

impl Sheet {
    pub fn new(name: impl Into<String>) -> Self {
        Sheet {
            name: name.into(),
            cells: BTreeMap::new(),
            origin: None,
            unread: 0,
        }
    }

    /// The used range: (rows, columns), 0 when empty.
    pub fn size(&self) -> (u32, u32) {
        let rows = self.cells.keys().map(|k| k.0 + 1).max().unwrap_or(0);
        let cols = self.cells.keys().map(|k| k.1 + 1).max().unwrap_or(0);
        (rows, cols)
    }

    pub fn get(&self, r: u32, c: u32) -> Option<&Cell> {
        self.cells.get(&(r, c))
    }

    /// `A1:D20`, or `A1` when empty.
    pub fn range(&self) -> String {
        let (r, c) = self.size();
        if r == 0 {
            return "A1".into();
        }
        format!("A1:{}", cell_ref(r - 1, c - 1))
    }
}

impl Cell {
    /// What a person sees in the cell.
    pub fn display(&self) -> String {
        match &self.value {
            Value::Empty => String::new(),
            Value::Text(s) => s.clone(),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
            Value::Error(e) => e.clone(),
            Value::Number(n) => format_number(*n, self.fmt),
        }
    }

    /// What editing the cell starts from: its formula, or its value as typed.
    pub fn input(&self) -> String {
        if let Some(f) = &self.formula {
            return format!("={f}");
        }
        match (&self.value, self.fmt) {
            (Value::Number(n), Fmt::Percent(_)) => format!("{}%", plain_number(n * 100.0)),
            (Value::Number(_), Fmt::Date | Fmt::DateTime) => self.display(),
            (Value::Number(n), _) => plain_number(*n),
            _ => self.display(),
        }
    }
}

pub fn plain_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{}", (n * 1e10).round() / 1e10);
        if s.contains('e') {
            format!("{n}")
        } else {
            s
        }
    }
}

fn format_number(n: f64, fmt: Fmt) -> String {
    match fmt {
        Fmt::General => plain_number(n),
        Fmt::Decimals(d) => format!("{:.*}", d as usize, n),
        Fmt::Grouped(d) => {
            let s = format!("{:.*}", d as usize, n.abs());
            let (int, frac) = s
                .split_once('.')
                .map_or((s.as_str(), None), |(i, f)| (i, Some(f)));
            let mut grouped = String::new();
            for (i, ch) in int.chars().enumerate() {
                if i > 0 && (int.len() - i) % 3 == 0 {
                    grouped.push(',');
                }
                grouped.push(ch);
            }
            let sign = if n < 0.0 && s.chars().any(|c| c != '0' && c != '.') {
                "-"
            } else {
                ""
            };
            match frac {
                Some(f) => format!("{sign}{grouped}.{f}"),
                None => format!("{sign}{grouped}"),
            }
        }
        Fmt::Percent(d) => format!("{:.*}%", d as usize, n * 100.0),
        Fmt::Date => serial_to_date(n).unwrap_or_else(|| plain_number(n)),
        Fmt::DateTime => match serial_to_date(n) {
            Some(d) => {
                let secs = ((n.fract() * 86400.0).round() as i64).rem_euclid(86400);
                if secs == 0 {
                    d
                } else {
                    format!("{d} {:02}:{:02}", secs / 3600, secs / 60 % 60)
                }
            }
            None => plain_number(n),
        },
    }
}

/// `2024-03-01` for a date serial (the 1900 system, as Excel and every spreadsheet use).
pub fn serial_to_date(n: f64) -> Option<String> {
    if !(1.0..2_958_466.0).contains(&n) {
        return None;
    }
    // Days from 1970-01-01: serial 25569 is 1970-01-01.
    let days = n.floor() as i64 - 25569;
    let (y, m, d) = civil_from_days(days);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// The serial of `2024-03-01` (also `2024/03/01`), when `s` is one.
pub fn date_to_serial(s: &str) -> Option<f64> {
    let s = s.trim();
    let parts: Vec<&str> = s.split(['-', '/']).collect();
    if parts.len() != 3 || parts[0].len() != 4 {
        return None;
    }
    let y: i64 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let d: u32 = parts[2].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((days_from_civil(y, m, d) + 25569) as f64)
}

// Howard Hinnant's date algorithms.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `B4` for (3, 1).
pub fn cell_ref(row: u32, col: u32) -> String {
    format!("{}{}", col_name(col), row + 1)
}

pub fn col_name(mut col: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// (row, column) of `B4` or `$B$4`.
pub fn parse_ref(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().replace('$', "").to_ascii_uppercase();
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    if letters.is_empty() || letters.len() > 3 || !letters.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let mut col = 0u32;
    for c in letters.bytes() {
        col = col * 26 + (c - b'A' + 1) as u32;
    }
    let row: u32 = digits.parse().ok()?;
    if row == 0 || row > 1_048_576 || col > 16_384 {
        return None;
    }
    Some((row - 1, col - 1))
}

/// ((r0, c0), (r1, c1)) of `A1:C3`, or of one cell.
pub fn parse_range(s: &str) -> Option<((u32, u32), (u32, u32))> {
    match s.split_once(':') {
        Some((a, b)) => {
            let (a, b) = (parse_ref(a)?, parse_ref(b)?);
            Some(((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1))))
        }
        None => parse_ref(s).map(|a| (a, a)),
    }
}

/// A value as a person or an agent typed it: `=` starts a formula; numbers, percentages,
/// booleans and dates (into a date cell) become values of their kind.
pub fn typed(input: &Json, fmt: Fmt) -> Cell {
    let mut cell = Cell {
        fmt,
        ..Cell::default()
    };
    match input {
        Json::Null => {}
        Json::Bool(b) => cell.value = Value::Bool(*b),
        Json::Number(n) => cell.value = Value::Number(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => {
            let t = s.trim();
            if let Some(f) = t.strip_prefix('=').filter(|f| !f.trim().is_empty()) {
                cell.formula = Some(f.trim().to_string());
            } else if t.is_empty() {
            } else if let Some(n) = parse_number(t) {
                cell.value = Value::Number(n);
            } else if let Some(p) = t.strip_suffix('%').and_then(parse_number) {
                cell.value = Value::Number(p / 100.0);
                if !matches!(fmt, Fmt::Percent(_)) {
                    cell.fmt = Fmt::Percent(if p.fract() == 0.0 { 0 } else { 1 });
                }
            } else if t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false") {
                cell.value = Value::Bool(t.eq_ignore_ascii_case("true"));
            } else if let Some(d) =
                date_to_serial(t).filter(|_| matches!(fmt, Fmt::Date | Fmt::DateTime))
            {
                cell.value = Value::Number(d);
            } else {
                cell.value = Value::Text(s.clone());
            }
        }
        other => cell.value = Value::Text(other.to_string()),
    }
    cell
}

/// `1234.5`, `-3`, `1e3`; not `12abc`, `0012` (an id), or a thousands-separated `1,234`.
pub fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t.len() > 30 {
        return None;
    }
    let digits = t.trim_start_matches(['-', '+']);
    if digits.len() > 1 && digits.starts_with('0') && !digits.starts_with("0.") {
        return None;
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
    {
        return None;
    }
    t.parse::<f64>().ok().filter(|n| n.is_finite())
}

// ---- documents ---------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Document {
    pub blocks: Vec<Block>,
    pub layout: DocumentLayout,
}

/// The document-wide base formatting. Direct paragraph, run and named-style values keep
/// precedence over these values. Margins are `None` when sections disagree.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentLayout {
    pub line_spacing: f64,
    pub paragraph_before: f64,
    pub paragraph_after: f64,
    pub character_spacing: f64,
    pub margin_top: Option<f64>,
    pub margin_right: Option<f64>,
    pub margin_bottom: Option<f64>,
    pub margin_left: Option<f64>,
    /// Exact margins for each section. The UI round-trips this when undoing a reset.
    pub section_margins: Vec<SectionMargins>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionMargins {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Default for DocumentLayout {
    fn default() -> Self {
        Self {
            line_spacing: 1.15,
            paragraph_before: 0.0,
            paragraph_after: 8.0,
            character_spacing: 0.0,
            margin_top: Some(1.0),
            margin_right: Some(1.0),
            margin_bottom: Some(1.0),
            margin_left: Some(1.0),
            section_margins: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct BlockLayout {
    pub line_spacing: f64,
    pub paragraph_before: f64,
    pub paragraph_after: f64,
    pub character_spacing: f64,
    pub line_override: bool,
    pub before_override: bool,
    pub after_override: bool,
    pub character_override: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    /// The text with inline marks as Markdown (`**bold**`, `*italic*`); a line break is `\n`.
    pub text: String,
    /// A table's cells.
    pub rows: Vec<Vec<String>>,
    /// Which block of the file this one was read from; `None` for a new block. A format copies
    /// an unchanged block's bytes as they were.
    pub origin: Option<usize>,
    /// Effective values for the page preview. Content equality intentionally ignores these.
    pub layout: BlockLayout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Heading(u8),
    Paragraph,
    Bullet(u8),
    Numbered(u8),
    Table,
    /// Something the model shows but does not edit (an image, a table of contents). It stays
    /// where it is unless deleted.
    Other(String),
}

impl Block {
    pub fn new(kind: BlockKind, text: impl Into<String>) -> Self {
        Block {
            kind,
            text: text.into(),
            rows: Vec::new(),
            origin: None,
            layout: BlockLayout::default(),
        }
    }

    pub fn table(rows: Vec<Vec<String>>) -> Self {
        Block {
            kind: BlockKind::Table,
            text: String::new(),
            rows,
            origin: None,
            layout: BlockLayout::default(),
        }
    }

    /// The block as one Markdown line (a table as several).
    pub fn to_markdown(&self) -> String {
        let text = self.text.replace('\n', "<br>");
        match &self.kind {
            BlockKind::Heading(n) => format!("{} {text}", "#".repeat(*n as usize)),
            BlockKind::Paragraph => text,
            BlockKind::Bullet(l) => format!("{}- {text}", "  ".repeat(*l as usize)),
            BlockKind::Numbered(l) => format!("{}1. {text}", "  ".repeat(*l as usize)),
            BlockKind::Table => table_markdown(&self.rows),
            BlockKind::Other(what) => format!("[{what}]"),
        }
    }

    /// True for an empty paragraph: spacing, not content. Kept, and left out of listings.
    pub fn is_blank(&self) -> bool {
        self.kind == BlockKind::Paragraph && self.text.trim().is_empty()
    }

    /// Same content (kind, text, cells) as `other`, wherever each came from.
    pub fn same(&self, other: &Block) -> bool {
        self.kind == other.kind && self.text == other.text && self.rows == other.rows
    }
}

fn table_markdown(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let line = |r: &Vec<String>| {
        let cells: Vec<String> = (0..cols)
            .map(|i| {
                r.get(i)
                    .map(|c| c.replace('|', "\\|").replace('\n', "<br>"))
                    .unwrap_or_default()
            })
            .collect();
        format!("| {} |", cells.join(" | "))
    };
    let mut out = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        out.push(line(r));
        if i == 0 {
            out.push(format!("|{}", " --- |".repeat(cols)));
        }
    }
    out.join("\n")
}

/// Markdown into blocks: `#` headings, `-`/`*` and `1.` list items (two spaces a level),
/// `|` tables, and every other non-empty line a paragraph. `<br>` is a line break.
pub fn parse_markdown(md: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let lines: Vec<&str> = md.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim_end();
        let t = line.trim_start();
        // A rule (`---`) separates; a document's headings already do.
        let rule = t.len() >= 3
            && t.chars().all(|c| matches!(c, '-' | '*' | '_' | ' '))
            && t.chars().filter(|c| *c != ' ').count() >= 3
            && t.chars()
                .filter(|c| *c != ' ')
                .collect::<std::collections::HashSet<_>>()
                .len()
                == 1;
        if t.is_empty() || rule {
            i += 1;
            continue;
        }
        if t.starts_with('|') {
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                let row = split_row(lines[i].trim());
                let separator = row
                    .iter()
                    .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')));
                if !separator {
                    rows.push(row);
                }
                i += 1;
            }
            out.push(Block::table(rows));
            continue;
        }
        i += 1;
        let indent = (line.len() - t.len()) as u8 / 2;
        let hashes = t.chars().take_while(|c| *c == '#').count();
        let block = if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            Block::new(BlockKind::Heading(hashes as u8), t[hashes..].trim())
        } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            Block::new(BlockKind::Bullet(indent.min(8)), rest.trim())
        } else if let Some(rest) = numbered(t) {
            Block::new(BlockKind::Numbered(indent.min(8)), rest.trim())
        } else {
            Block::new(BlockKind::Paragraph, t)
        };
        out.push(Block {
            text: block.text.replace("<br>", "\n"),
            ..block
        });
    }
    out
}

fn numbered(t: &str) -> Option<&str> {
    let n = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if n == 0 || n > 3 {
        return None;
    }
    t[n..]
        .strip_prefix(". ")
        .or_else(|| t[n..].strip_prefix(") "))
}

fn split_row(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|');
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cur.push('|');
            chars.next();
        } else if c == '|' {
            cells.push(cur.trim().replace("<br>", "\n"));
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().replace("<br>", "\n"));
    cells
}

/// Inline Markdown as runs: (text, bold, italic). Only `**` and `*` (or `_`) are marks.
pub fn inline_runs(text: &str) -> Vec<(String, bool, bool)> {
    let mut out: Vec<(String, bool, bool)> = Vec::new();
    let (mut bold, mut italic) = (false, false);
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let push = |out: &mut Vec<(String, bool, bool)>, cur: &mut String, b: bool, it: bool| {
        if !cur.is_empty() {
            out.push((std::mem::take(cur), b, it));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && matches!(chars[i + 1], '*' | '_' | '\\') {
            cur.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            // A mark only where it can open or close.
            if bold || closes(&chars, i + 2, "**") {
                push(&mut out, &mut cur, bold, italic);
                bold = !bold;
                i += 2;
                continue;
            }
        } else if (c == '*' || c == '_') && (italic || closes(&chars, i + 1, &c.to_string())) {
            let word_inner = c == '_'
                && i > 0
                && chars[i - 1].is_alphanumeric()
                && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric());
            if !word_inner {
                push(&mut out, &mut cur, bold, italic);
                italic = !italic;
                i += 1;
                continue;
            }
        }
        cur.push(c);
        i += 1;
    }
    push(&mut out, &mut cur, bold, italic);
    out
}

/// Whether a mark at `from` has a closing `mark` later, with text between.
fn closes(chars: &[char], from: usize, mark: &str) -> bool {
    if chars.get(from).is_none_or(|c| c.is_whitespace()) {
        return false;
    }
    let rest: String = chars[from + 1..].iter().collect();
    rest.contains(mark)
}

/// Runs back to inline Markdown.
pub fn runs_markdown(runs: &[(String, bool, bool)]) -> String {
    let mut out = String::new();
    for (t, b, i) in runs {
        if t.is_empty() {
            continue;
        }
        let core = t.trim();
        if core.is_empty() {
            out.push_str(t);
            continue;
        }
        let lead = t.len() - t.trim_start().len();
        let trail = t.len() - t.trim_end().len();
        out.push_str(&t[..lead]);
        let core = core.replace('*', "\\*");
        match (b, i) {
            (true, true) => out.push_str(&format!("***{core}***")),
            (true, false) => out.push_str(&format!("**{core}**")),
            (false, true) => out.push_str(&format!("*{core}*")),
            _ => out.push_str(&core),
        }
        out.push_str(&t[t.len() - trail..]);
    }
    out
}

/// Text without inline marks.
pub fn plain(text: &str) -> String {
    inline_runs(text).into_iter().map(|r| r.0).collect()
}

// ---- edits -------------------------------------------------------------------------------------

/// One change to a file. The agent sends these (`files.edit`) and so does the window when the
/// person edits: one path for both.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Cells from `at` (top-left), row by row. A value starting with `=` is a formula.
    Set {
        sheet: Option<String>,
        at: String,
        rows: Vec<Vec<Json>>,
    },
    Clear {
        sheet: Option<String>,
        range: String,
    },
    AddSheet {
        name: String,
        rows: Vec<Vec<Json>>,
    },
    DeleteSheet {
        name: String,
    },
    RenameSheet {
        from: String,
        to: String,
    },
    /// Block `block` becomes the blocks of `text` (Markdown); empty text deletes it.
    Replace {
        block: usize,
        text: String,
    },
    /// The blocks of `text` go after block `after` (0: at the start).
    Insert {
        after: usize,
        text: String,
    },
    Delete {
        block: usize,
    },
    /// Document-wide base formatting. Omitted values stay as they are. Margins are inches;
    /// paragraph and character spacing are points.
    Layout {
        line_spacing: Option<f64>,
        paragraph_before: Option<f64>,
        paragraph_after: Option<f64>,
        character_spacing: Option<f64>,
        margin_top: Option<f64>,
        margin_right: Option<f64>,
        margin_bottom: Option<f64>,
        margin_left: Option<f64>,
        /// Internal UI restore token. The compact agent format intentionally omits this.
        section_margins: Option<Vec<SectionMargins>>,
    },
    /// Text replaced wherever it appears (in a text file, every cell, every block).
    FindReplace {
        find: String,
        replace: String,
    },
}

impl Edit {
    /// One edit from the JSON an agent writes. Errors say what shape was expected.
    pub fn parse(v: &Json) -> Result<Edit, Error> {
        let s = |k: &str| v.get(k).and_then(Json::as_str).map(str::to_string);
        let n = |k: &str| v.get(k).and_then(Json::as_u64).map(|x| x as usize);
        let f = |v: &Json, k: &str| v.get(k).and_then(Json::as_f64);
        let sheet = s("sheet");
        let rows = |v: &Json| -> Vec<Vec<Json>> {
            match v.get("rows").or_else(|| v.get("values")) {
                Some(Json::Array(rows)) => rows
                    .iter()
                    .map(|r| match r {
                        Json::Array(cells) => cells.clone(),
                        other => vec![other.clone()],
                    })
                    .collect(),
                _ => match v.get("value") {
                    Some(x) => vec![vec![x.clone()]],
                    None => Vec::new(),
                },
            }
        };
        if let Some(find) = s("find") {
            return Ok(Edit::FindReplace {
                find,
                replace: s("replace").unwrap_or_default(),
            });
        }
        if let Some(name) = s("add_sheet") {
            return Ok(Edit::AddSheet {
                name,
                rows: rows(v),
            });
        }
        if let Some(name) = s("delete_sheet") {
            return Ok(Edit::DeleteSheet { name });
        }
        if let (Some(from), Some(to)) = (s("rename_sheet"), s("to")) {
            return Ok(Edit::RenameSheet { from, to });
        }
        if let Some(range) = s("clear") {
            return Ok(Edit::Clear { sheet, range });
        }
        if let Some(at) = s("cell").or_else(|| s("at")) {
            let rows = rows(v);
            if rows.is_empty() {
                return Err(Error::Input(format!(
                    "the edit at {at} needs `value` or `rows`"
                )));
            }
            return Ok(Edit::Set { sheet, at, rows });
        }
        if let Some(block) = n("delete") {
            return Ok(Edit::Delete { block });
        }
        if let Some(block) = n("block") {
            return Ok(Edit::Replace {
                block,
                text: s("text").unwrap_or_default(),
            });
        }
        if let Some(after) = n("after") {
            return Ok(Edit::Insert {
                after,
                text: s("text").unwrap_or_default(),
            });
        }
        if let (Some(key), Some(value)) = (s("format"), f(v, "value")) {
            let mut values = [None; 8];
            let index = match key.as_str() {
                "line_spacing" => 0,
                "paragraph_before" => 1,
                "paragraph_after" => 2,
                "character_spacing" => 3,
                "margin_top" => 4,
                "margin_right" => 5,
                "margin_bottom" => 6,
                "margin_left" => 7,
                _ => return Err(Error::Input(format!("unknown document format `{key}`"))),
            };
            values[index] = Some(value);
            return Ok(Edit::Layout {
                line_spacing: values[0],
                paragraph_before: values[1],
                paragraph_after: values[2],
                character_spacing: values[3],
                margin_top: values[4],
                margin_right: values[5],
                margin_bottom: values[6],
                margin_left: values[7],
                section_margins: None,
            });
        }
        if let Some(layout) = v.get("layout").and_then(Json::as_object) {
            let layout = Json::Object(layout.clone());
            let change = Edit::Layout {
                line_spacing: f(&layout, "line_spacing"),
                paragraph_before: f(&layout, "paragraph_before"),
                paragraph_after: f(&layout, "paragraph_after"),
                character_spacing: f(&layout, "character_spacing"),
                margin_top: f(&layout, "margin_top"),
                margin_right: f(&layout, "margin_right"),
                margin_bottom: f(&layout, "margin_bottom"),
                margin_left: f(&layout, "margin_left"),
                section_margins: layout.get("section_margins").and_then(Json::as_array).map(
                    |sections| {
                        sections
                            .iter()
                            .filter_map(|section| {
                                Some(SectionMargins {
                                    top: section.get("top")?.as_f64()?,
                                    right: section.get("right")?.as_f64()?,
                                    bottom: section.get("bottom")?.as_f64()?,
                                    left: section.get("left")?.as_f64()?,
                                })
                            })
                            .collect()
                    },
                ),
            };
            if matches!(
                &change,
                Edit::Layout {
                    line_spacing: None,
                    paragraph_before: None,
                    paragraph_after: None,
                    character_spacing: None,
                    margin_top: None,
                    margin_right: None,
                    margin_bottom: None,
                    margin_left: None,
                    section_margins: None,
                }
            ) {
                return Err(Error::Input("`layout` has no formatting values".into()));
            }
            return Ok(change);
        }
        Err(Error::Input(format!(
            "an edit is one of: {{cell, value}} or {{cell, rows}} (spreadsheets), {{clear}}, {{add_sheet, rows}}, \
             {{block, text}}, {{after, text}}, {{delete}}, {{format, value}} (documents), {{find, replace}} (any file); got {v}"
        )))
    }

    pub fn parse_all(v: &Json) -> Result<Vec<Edit>, Error> {
        match v {
            Json::Array(a) => a.iter().map(Edit::parse).collect(),
            Json::Object(_) => Ok(vec![Edit::parse(v)?]),
            _ => Err(Error::Input("`edits` is a list of edits".into())),
        }
    }

    /// The edit in a few words, for an approval or an activity line.
    pub fn describe(&self) -> String {
        let clip = |s: &str, n: usize| {
            let one = s.replace('\n', " ");
            if one.chars().count() > n {
                format!("{}…", one.chars().take(n).collect::<String>())
            } else {
                one
            }
        };
        let on =
            |sheet: &Option<String>| sheet.as_ref().map(|s| format!("{s}!")).unwrap_or_default();
        match self {
            Edit::Set { sheet, at, rows } => {
                let cells: usize = rows.iter().map(Vec::len).sum();
                if cells == 1 {
                    let v = &rows[0][0];
                    let shown = v
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string());
                    format!("{}{at} = {}", on(sheet), clip(&shown, 40))
                } else {
                    format!("{cells} cells from {}{at}", on(sheet))
                }
            }
            Edit::Clear { sheet, range } => format!("clear {}{range}", on(sheet)),
            Edit::AddSheet { name, rows } => match rows.len() {
                0 => format!("add sheet {name}"),
                n => format!("add sheet {name} ({n} rows)"),
            },
            Edit::DeleteSheet { name } => format!("delete sheet {name}"),
            Edit::RenameSheet { from, to } => format!("rename sheet {from} to {to}"),
            Edit::Replace { block, text } if text.trim().is_empty() => {
                format!("delete block {block}")
            }
            Edit::Replace { block, text }
                if text.lines().count() > 1 || text.chars().count() > 60 =>
            {
                format!("replace block {block} with:")
            }
            Edit::Insert { after, text }
                if text.lines().count() > 1 || text.chars().count() > 60 =>
            {
                match after {
                    0 => "insert at the start:".to_string(),
                    n => format!("insert after block {n}:"),
                }
            }
            Edit::Replace { block, text } => format!("block {block}: {}", clip(text, 60)),
            Edit::Insert { after, text } => format!("after block {after}: {}", clip(text, 60)),
            Edit::Delete { block } => format!("delete block {block}"),
            Edit::Layout { .. } => "change document formatting".into(),
            Edit::FindReplace { find, replace } => {
                format!("\"{}\" to \"{}\"", clip(find, 30), clip(replace, 30))
            }
        }
    }
}

fn wrong(kind: Kind, e: &Edit) -> Error {
    let hint = match kind {
        Kind::Sheet => "a spreadsheet takes {cell, value}, {cell, rows}, {clear}, {add_sheet}, {delete_sheet}, {rename_sheet} and {find, replace}",
        Kind::Document => "a document takes {block, text}, {after, text}, {delete}, {layout} and {find, replace}",
        Kind::Text | Kind::Markdown | Kind::Map => {
            "a text file takes {find, replace}, or files.write with the whole text"
        }
        Kind::Pdf | Kind::Image => "ARK does not change a PDF or an image; write a new file",
        Kind::Model => "ARK does not edit model bytes; regenerate the file from its source",
    };
    Error::Input(format!("`{}` does not apply here: {hint}", e.describe()))
}

/// Apply edits to a workbook. Sheet names match without case; no name means the first sheet.
pub fn apply_sheet(book: &mut Workbook, edits: &[Edit]) -> Result<(), Error> {
    fn find(book: &Workbook, name: &Option<String>) -> Result<usize, Error> {
        match name {
            None => {
                if book.sheets.is_empty() {
                    Err(Error::Input("the workbook has no sheet".into()))
                } else {
                    Ok(0)
                }
            }
            Some(n) => book
                .sheets
                .iter()
                .position(|s| s.name.eq_ignore_ascii_case(n.trim()))
                .ok_or_else(|| {
                    let names: Vec<&str> = book.sheets.iter().map(|s| s.name.as_str()).collect();
                    Error::Input(format!(
                        "no sheet named {n}; the sheets are {}",
                        names.join(", ")
                    ))
                }),
        }
    }
    fn put(sheet: &mut Sheet, r0: u32, c0: u32, rows: &[Vec<Json>]) {
        for (dr, row) in rows.iter().enumerate() {
            for (dc, v) in row.iter().enumerate() {
                let key = (r0 + dr as u32, c0 + dc as u32);
                let fmt = sheet.cells.get(&key).map(|c| c.fmt).unwrap_or_default();
                let cell = typed(v, fmt);
                if cell.value == Value::Empty && cell.formula.is_none() {
                    sheet.cells.remove(&key);
                } else {
                    sheet.cells.insert(key, cell);
                }
            }
        }
    }
    for e in edits {
        match e {
            Edit::Set { sheet, at, rows } => {
                let i = find(book, sheet)?;
                let (r, c) = parse_ref(at)
                    .ok_or_else(|| Error::Input(format!("`{at}` is not a cell like B4")))?;
                put(&mut book.sheets[i], r, c, rows);
            }
            Edit::Clear { sheet, range } => {
                let i = find(book, sheet)?;
                let ((r0, c0), (r1, c1)) = parse_range(range)
                    .ok_or_else(|| Error::Input(format!("`{range}` is not a range like A2:C9")))?;
                book.sheets[i]
                    .cells
                    .retain(|(r, c), _| !(*r >= r0 && *r <= r1 && *c >= c0 && *c <= c1));
            }
            Edit::AddSheet { name, rows } => {
                let name = name.trim();
                if name.is_empty()
                    || name.len() > 31
                    || name.contains(['[', ']', ':', '*', '?', '/', '\\'])
                {
                    return Err(Error::Input(format!(
                        "`{name}` cannot name a sheet (1 to 31 characters, none of [ ] : * ? / \\)"
                    )));
                }
                if book
                    .sheets
                    .iter()
                    .any(|s| s.name.eq_ignore_ascii_case(name))
                {
                    return Err(Error::Input(format!(
                        "there is already a sheet named {name}"
                    )));
                }
                let mut s = Sheet::new(name);
                put(&mut s, 0, 0, rows);
                book.sheets.push(s);
            }
            Edit::DeleteSheet { name } => {
                let i = find(book, &Some(name.clone()))?;
                if book.sheets.len() == 1 {
                    return Err(Error::Input("a workbook keeps at least one sheet".into()));
                }
                book.sheets.remove(i);
            }
            Edit::RenameSheet { from, to } => {
                let i = find(book, &Some(from.clone()))?;
                if book
                    .sheets
                    .iter()
                    .enumerate()
                    .any(|(j, s)| j != i && s.name.eq_ignore_ascii_case(to))
                {
                    return Err(Error::Input(format!("there is already a sheet named {to}")));
                }
                let from_name = book.sheets[i].name.clone();
                book.sheets[i].name = to.trim().to_string();
                for s in &mut book.sheets {
                    for cell in s.cells.values_mut() {
                        if let Some(f) = &cell.formula {
                            cell.formula =
                                Some(crate::formula::rename_sheet(f, &from_name, to.trim()));
                        }
                    }
                }
            }
            Edit::FindReplace { find: f, replace } => {
                if f.is_empty() {
                    return Err(Error::Input("`find` is empty".into()));
                }
                let mut hit = false;
                for s in &mut book.sheets {
                    for cell in s.cells.values_mut() {
                        if let Value::Text(t) = &cell.value {
                            if cell.formula.is_none() && t.contains(f.as_str()) {
                                cell.value = Value::Text(t.replace(f.as_str(), replace));
                                hit = true;
                            }
                        }
                    }
                }
                if !hit {
                    return Err(Error::Input(format!("no cell contains \"{f}\"")));
                }
            }
            other => return Err(wrong(Kind::Sheet, other)),
        }
    }
    crate::formula::recalc(book);
    Ok(())
}

/// Apply edits to a document. Block numbers are the ones a read showed (from 1), before any of
/// these edits; edits run from the last block to the first so earlier numbers stay right.
pub fn apply_document(doc: &mut Document, edits: &[Edit]) -> Result<(), Error> {
    let total = doc.blocks.len();
    let check = |n: usize, zero_ok: bool| -> Result<(), Error> {
        if (n == 0 && !zero_ok) || n > total {
            Err(Error::Input(format!(
                "there is no block {n}; the document has blocks 1 to {total}"
            )))
        } else {
            Ok(())
        }
    };
    let mut keyed: Vec<(usize, u8, &Edit)> = Vec::new();
    for e in edits {
        match e {
            Edit::Replace { block, .. } | Edit::Delete { block } => {
                check(*block, false)?;
                keyed.push((*block, 1, e));
            }
            Edit::Insert { after, .. } => {
                check(*after, true)?;
                keyed.push((*after, 0, e));
            }
            Edit::FindReplace { .. } => {}
            Edit::Layout { .. } => {}
            other => return Err(wrong(Kind::Document, other)),
        }
    }
    // Last block first; at one position the insert after it runs before its replacement, so
    // "replace 3, insert after 3" puts the new blocks after the new 3.
    keyed.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut seen = std::collections::HashSet::new();
    for (n, _, e) in keyed {
        match e {
            Edit::Insert { text, .. } => {
                let new = parse_markdown(text);
                doc.blocks.splice(n..n, new);
            }
            Edit::Replace { .. } | Edit::Delete { .. } => {
                if !seen.insert(n) {
                    return Err(Error::Input(format!("block {n} is changed twice")));
                }
                let text = match e {
                    Edit::Replace { text, .. } => text.as_str(),
                    _ => "",
                };
                let mut new = parse_markdown(text);
                // A changed block keeps its place in the file, so the format can keep its
                // look (the paragraph's style, a table's borders).
                if let Some(first) = new.first_mut() {
                    first.origin = doc.blocks[n - 1].origin;
                }
                doc.blocks.splice(n - 1..n, new);
            }
            _ => {}
        }
    }
    for e in edits {
        if let Edit::Layout {
            line_spacing,
            paragraph_before,
            paragraph_after,
            character_spacing,
            margin_top,
            margin_right,
            margin_bottom,
            margin_left,
            section_margins,
        } = e
        {
            let valid = |name: &str, v: Option<f64>, min: f64, max: f64| -> Result<(), Error> {
                if let Some(v) = v {
                    if !v.is_finite() || v < min || v > max {
                        return Err(Error::Input(format!("{name} must be from {min} to {max}")));
                    }
                }
                Ok(())
            };
            valid("line spacing", *line_spacing, 0.5, 4.0)?;
            valid("paragraph before", *paragraph_before, 0.0, 144.0)?;
            valid("paragraph after", *paragraph_after, 0.0, 144.0)?;
            valid("character spacing", *character_spacing, -2.0, 20.0)?;
            for (name, value) in [
                ("top margin", margin_top),
                ("right margin", margin_right),
                ("bottom margin", margin_bottom),
                ("left margin", margin_left),
            ] {
                valid(name, *value, 0.0, 6.0)?;
            }
            if let Some(sections) = section_margins {
                for section in sections {
                    for (name, value) in [
                        ("top margin", section.top),
                        ("right margin", section.right),
                        ("bottom margin", section.bottom),
                        ("left margin", section.left),
                    ] {
                        valid(name, Some(value), 0.0, 6.0)?;
                    }
                }
            }
            if let Some(v) = line_spacing {
                doc.layout.line_spacing = *v;
            }
            if let Some(v) = paragraph_before {
                doc.layout.paragraph_before = *v;
            }
            if let Some(v) = paragraph_after {
                doc.layout.paragraph_after = *v;
            }
            if let Some(v) = character_spacing {
                doc.layout.character_spacing = *v;
            }
            if let Some(v) = margin_top {
                doc.layout.margin_top = Some(*v);
            }
            if let Some(v) = margin_right {
                doc.layout.margin_right = Some(*v);
            }
            if let Some(v) = margin_bottom {
                doc.layout.margin_bottom = Some(*v);
            }
            if let Some(v) = margin_left {
                doc.layout.margin_left = Some(*v);
            }
            if let Some(v) = section_margins {
                doc.layout.section_margins = v.clone();
                let same = |pick: fn(&SectionMargins) -> f64| {
                    v.first().map(pick).filter(|first| {
                        v.iter()
                            .all(|section| (pick(section) - *first).abs() < 0.0001)
                    })
                };
                doc.layout.margin_top = same(|m| m.top);
                doc.layout.margin_right = same(|m| m.right);
                doc.layout.margin_bottom = same(|m| m.bottom);
                doc.layout.margin_left = same(|m| m.left);
            }
        }
        if let Edit::FindReplace { find, replace } = e {
            if find.is_empty() {
                return Err(Error::Input("`find` is empty".into()));
            }
            let mut hit = false;
            for b in &mut doc.blocks {
                if b.text.contains(find.as_str()) {
                    b.text = b.text.replace(find.as_str(), replace);
                    hit = true;
                }
                for cell in b.rows.iter_mut().flatten() {
                    if cell.contains(find.as_str()) {
                        *cell = cell.replace(find.as_str(), replace);
                        hit = true;
                    }
                }
            }
            if !hit {
                return Err(Error::Input(format!(
                    "the document does not contain \"{find}\""
                )));
            }
        }
    }
    Ok(())
}

/// Apply edits to text.
pub fn apply_text(text: &mut String, edits: &[Edit]) -> Result<(), Error> {
    for e in edits {
        match e {
            Edit::FindReplace { find, replace } => {
                if find.is_empty() {
                    return Err(Error::Input("`find` is empty".into()));
                }
                if !text.contains(find.as_str()) {
                    return Err(Error::Input(format!(
                        "the file does not contain \"{find}\""
                    )));
                }
                *text = text.replace(find.as_str(), replace);
            }
            other => return Err(wrong(Kind::Text, other)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cell_names_and_dates() {
        assert_eq!(cell_ref(3, 1), "B4");
        assert_eq!(col_name(26), "AA");
        assert_eq!(col_name(701), "ZZ");
        assert_eq!(parse_ref("$AA$10"), Some((9, 26)));
        assert_eq!(parse_range("C3:A1"), Some(((0, 0), (2, 2))));
        assert_eq!(serial_to_date(45352.0).as_deref(), Some("2024-03-01"));
        assert_eq!(date_to_serial("2024-03-01"), Some(45352.0));
        assert_eq!(parse_number("0012"), None);
        assert_eq!(parse_number("-1.5"), Some(-1.5));
    }

    #[test]
    fn typed_values_take_their_kind() {
        assert_eq!(
            typed(&json!("12.5"), Fmt::General).value,
            Value::Number(12.5)
        );
        assert_eq!(
            typed(&json!("=SUM(A1:A3)"), Fmt::General)
                .formula
                .as_deref(),
            Some("SUM(A1:A3)")
        );
        let p = typed(&json!("15%"), Fmt::General);
        assert_eq!((p.value, p.fmt), (Value::Number(0.15), Fmt::Percent(0)));
        assert_eq!(
            typed(&json!("2024-03-01"), Fmt::Date).value,
            Value::Number(45352.0)
        );
        assert_eq!(
            typed(&json!("2024-03-01"), Fmt::General).value,
            Value::Text("2024-03-01".into())
        );
    }

    #[test]
    fn markdown_round_trips_through_blocks() {
        let md = "# Sales\n---\nThe **north** grew.\n\n- one\n  - two\n1. first\n\n| A | B |\n|---|---|\n| 1 | 2 |";
        let blocks = parse_markdown(md);
        let kinds: Vec<&BlockKind> = blocks.iter().map(|b| &b.kind).collect();
        assert_eq!(
            kinds,
            [
                &BlockKind::Heading(1),
                &BlockKind::Paragraph,
                &BlockKind::Bullet(0),
                &BlockKind::Bullet(1),
                &BlockKind::Numbered(0),
                &BlockKind::Table
            ]
        );
        assert_eq!(blocks[5].rows, vec![vec!["A", "B"], vec!["1", "2"]]);
        let back: Vec<String> = blocks.iter().map(Block::to_markdown).collect();
        assert_eq!(parse_markdown(&back.join("\n")), blocks);
        assert_eq!(
            inline_runs("The **north** *grew*"),
            vec![
                ("The ".into(), false, false),
                ("north".into(), true, false),
                (" ".into(), false, false),
                ("grew".into(), false, true)
            ]
        );
        assert_eq!(
            inline_runs("snake_case_name and 2 * 3"),
            vec![("snake_case_name and 2 * 3".into(), false, false)]
        );
        assert_eq!(runs_markdown(&inline_runs("a **b** c")), "a **b** c");
        assert_eq!(
            inline_runs("*é*"),
            vec![("é".into(), false, true)],
            "marks around a wide character"
        );
    }

    #[test]
    fn a_run_of_only_spaces_keeps_its_spaces() {
        let runs = [
            ("a".to_string(), true, false),
            (" ".to_string(), false, false),
            ("  ".to_string(), true, true),
            ("b".to_string(), false, false),
        ];
        assert_eq!(runs_markdown(&runs), "**a**   b");
    }

    #[test]
    fn document_edits_use_the_numbers_of_the_read() {
        let mut doc = Document {
            blocks: parse_markdown("# T\none\ntwo\nthree")
                .into_iter()
                .enumerate()
                .map(|(i, b)| Block {
                    origin: Some(i),
                    ..b
                })
                .collect(),
            ..Document::default()
        };
        let edits = Edit::parse_all(&json!([
            {"block": 2, "text": "ONE"},
            {"after": 2, "text": "one and a half"},
            {"delete": 4},
            {"after": 0, "text": "Draft"}
        ]))
        .unwrap();
        apply_document(&mut doc, &edits).unwrap();
        let texts: Vec<&str> = doc.blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["Draft", "T", "ONE", "one and a half", "two"]);
        assert_eq!(
            doc.blocks[2].origin,
            Some(1),
            "a replaced block keeps its place"
        );
        assert_eq!(doc.blocks[3].origin, None);
        let format = Edit::parse_all(&json!([
            {"format": "line_spacing", "value": 1.5},
            {"format": "margin_left", "value": 1.25}
        ]))
        .unwrap();
        apply_document(&mut doc, &format).unwrap();
        assert_eq!(doc.layout.line_spacing, 1.5);
        assert_eq!(doc.layout.margin_left, Some(1.25));
        assert!(
            apply_document(&mut doc, &Edit::parse_all(&json!([{"delete": 9}])).unwrap()).is_err()
        );
    }

    #[test]
    fn sheet_edits_and_errors() {
        let mut book = Workbook {
            sheets: vec![Sheet::new("Sales")],
            ..Workbook::default()
        };
        let edits = Edit::parse_all(&json!([
            {"cell": "A1", "rows": [["Region", "Q1"], ["North", 10], ["South", "20"]]},
            {"cell": "B4", "value": "=SUM(B2:B3)"},
            {"add_sheet": "Summary", "rows": [["Total", "=Sales!B4"]]}
        ]))
        .unwrap();
        apply_sheet(&mut book, &edits).unwrap();
        assert_eq!(book.sheets[0].get(3, 1).unwrap().value, Value::Number(30.0));
        assert_eq!(book.sheets[1].get(0, 1).unwrap().value, Value::Number(30.0));
        let bad = Edit::parse_all(&json!([{"sheet": "Nope", "cell": "A1", "value": 1}])).unwrap();
        let e = apply_sheet(&mut book, &bad).unwrap_err().to_string();
        assert!(e.contains("Sales, Summary"), "{e}");
        let wrong = Edit::parse_all(&json!([{"block": 1, "text": "x"}])).unwrap();
        assert!(apply_sheet(&mut book, &wrong).is_err());
        assert!(Edit::parse(&json!({"what": 1})).is_err());
    }
}
