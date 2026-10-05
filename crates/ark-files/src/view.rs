//! Two views of a file: the text an agent reads (bounded, addressable: cells by name, blocks
//! by number) and the JSON a window draws.

use serde_json::{json, Value as Json};

use crate::model::{cell_ref, col_name, parse_range, BlockKind, Content, Sheet, Value};
use crate::File;

/// Characters of a file the agent gets in one read; the rest is read with a range.
pub const MAX_LISTING: usize = 12_000;
/// Characters of one cell in a listing.
const CELL_CHARS: usize = 80;
/// Rows and columns a window gets per sheet.
const PAGE_ROWS: u32 = 5_000;
const PAGE_COLS: u32 = 200;

/// What a read shows the agent.
pub struct Listing {
    pub text: String,
    /// Where the next read starts, when this one stopped early (`A41:D200`, a block, a line).
    pub more: Option<String>,
}

/// Where a read starts and what it covers.
#[derive(Default)]
pub struct Window {
    pub sheet: Option<String>,
    pub range: Option<String>,
    /// First block (documents) or line (text), from 1.
    pub from: Option<usize>,
}

impl File {
    /// The file as an agent reads it.
    pub fn listing(&self, w: &Window) -> Result<Listing, crate::Error> {
        match &self.content {
            Content::Text(t) => Ok(text_listing(self, t, w.from.unwrap_or(1))),
            Content::Sheet(book) => {
                let mut head = format!(
                    "{}: spreadsheet, {} sheet{}: {}\n",
                    self.path,
                    book.sheets.len(),
                    if book.sheets.len() == 1 { "" } else { "s" },
                    book.sheets
                        .iter()
                        .map(|s| format!("{} ({})", s.name, s.range()))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let chosen: Vec<&Sheet> = match &w.sheet {
                    Some(n) => {
                        let s = book
                            .sheets
                            .iter()
                            .find(|s| s.name.eq_ignore_ascii_case(n.trim()))
                            .ok_or_else(|| {
                                crate::Error::Input(format!(
                                    "no sheet named {n}; the sheets are {}",
                                    book.sheets
                                        .iter()
                                        .map(|s| s.name.as_str())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ))
                            })?;
                        vec![s]
                    }
                    None => book.sheets.iter().collect(),
                };
                let mut more = None;
                for s in chosen {
                    let room = MAX_LISTING.saturating_sub(head.len());
                    let (text, rest) = sheet_listing(s, w.range.as_deref(), room)?;
                    head.push('\n');
                    head.push_str(&text);
                    if let Some(r) = rest {
                        more.get_or_insert(r);
                        break;
                    }
                }
                Ok(Listing { text: head, more })
            }
            Content::Document(doc) => {
                let from = w.from.unwrap_or(1).max(1);
                let shown = doc.blocks.iter().filter(|b| !b.is_blank()).count();
                let mut out = format!(
                    "{}: document, {} block{}\n(the numbers are what files.edit takes)\n\n",
                    self.path,
                    shown,
                    if shown == 1 { "" } else { "s" }
                );
                let mut more = None;
                for (i, b) in doc.blocks.iter().enumerate().skip(from - 1) {
                    if b.is_blank() {
                        continue;
                    }
                    let md = b.to_markdown().replace('\n', "\n    ");
                    let line = format!("[{}] {md}\n", i + 1);
                    if out.len() + line.len() > MAX_LISTING {
                        more = Some(format!("from {}", i + 1));
                        out.push_str(&format!(
                            "[blocks {}-{} not shown: read with from {}]\n",
                            i + 1,
                            doc.blocks.len(),
                            i + 1
                        ));
                        break;
                    }
                    out.push_str(&line);
                }
                if doc.blocks.is_empty() {
                    out.push_str("(empty)\n");
                }
                Ok(Listing { text: out, more })
            }
        }
    }

    /// The file as a window draws it.
    pub fn page(&self) -> Json {
        let body = match &self.content {
            Content::Text(t) => json!({ "text": t }),
            Content::Sheet(book) => json!({
                "sheets": book.sheets.iter().map(sheet_page).collect::<Vec<_>>()
            }),
            Content::Document(doc) => json!({
                "blocks": doc.blocks.iter().map(|b| {
                    let (kind, level) = match &b.kind {
                        BlockKind::Heading(n) => ("heading", *n),
                        BlockKind::Paragraph => ("paragraph", 0),
                        BlockKind::Bullet(l) => ("bullet", *l),
                        BlockKind::Numbered(l) => ("numbered", *l),
                        BlockKind::Table => ("table", 0),
                        BlockKind::Other(_) => ("other", 0),
                    };
                    let mut v = json!({ "kind": kind, "level": level, "text": b.text });
                    if let BlockKind::Other(what) = &b.kind {
                        v["label"] = json!(what);
                    }
                    if !b.rows.is_empty() {
                        v["rows"] = json!(b.rows);
                    }
                    v
                }).collect::<Vec<_>>()
            }),
        };
        let mut v = json!({
            "path": self.path,
            "name": self.name(),
            "kind": self.kind().as_str(),
            "format": self.format,
            "version": self.version,
            "size": self.size,
        });
        if let (Some(obj), Json::Object(b)) = (v.as_object_mut(), body) {
            obj.extend(b);
        }
        v
    }
}

fn text_listing(f: &File, t: &str, from: usize) -> Listing {
    let from = from.max(1);
    let lines = t.lines().count();
    let mut body = String::new();
    let mut last = from - 1;
    for (i, line) in t.lines().enumerate().skip(from - 1) {
        if body.len() + line.len() + 1 > MAX_LISTING {
            break;
        }
        body.push_str(line);
        body.push('\n');
        last = i + 1;
    }
    let mut text = format!("{} (lines {from}-{last} of {lines})\n{body}", f.path);
    let more = (last < lines).then(|| {
        text.push_str(&format!("[more: read again with from {}]", last + 1));
        format!("from {}", last + 1)
    });
    Listing { text, more }
}

fn cell_text(s: &Sheet, r: u32, c: u32) -> String {
    let Some(cell) = s.get(r, c) else {
        return String::new();
    };
    let mut t = cell.display().replace(['\n', '|'], " ");
    if t.chars().count() > CELL_CHARS {
        t = t.chars().take(CELL_CHARS).collect::<String>() + "…";
    }
    match &cell.formula {
        Some(f) => format!("{t} [={f}]"),
        None => t,
    }
}

/// A sheet as rows `n | a | b`, starting at `range`. Returns the text and, when it stopped
/// early, the range to read next.
fn sheet_listing(
    s: &Sheet,
    range: Option<&str>,
    room: usize,
) -> Result<(String, Option<String>), crate::Error> {
    let (rows, cols) = s.size();
    let ((r0, c0), (r1, c1)) = match range {
        Some(r) => parse_range(r)
            .ok_or_else(|| crate::Error::Input(format!("`{r}` is not a range like A1:D40")))?,
        None => ((0, 0), (rows.saturating_sub(1), cols.saturating_sub(1))),
    };
    let mut out = format!(
        "{}!{}\n",
        s.name,
        if rows == 0 {
            "(empty)".to_string()
        } else {
            format!("{}:{}", cell_ref(r0, c0), cell_ref(r1, c1))
        }
    );
    if rows == 0 {
        return Ok((out, None));
    }
    let c1 = c1.min(cols.saturating_sub(1)).max(c0);
    let header: Vec<String> = (c0..=c1).map(col_name).collect();
    out.push_str(&format!("  | {}\n", header.join(" | ")));
    for r in r0..=r1.min(rows - 1) {
        if s.cells.range((r, c0)..=(r, c1)).next().is_none() {
            continue;
        }
        let cells: Vec<String> = (c0..=c1).map(|c| cell_text(s, r, c)).collect();
        let line = format!(
            "{} | {}\n",
            r + 1,
            cells.join(" | ").trim_end_matches([' ', '|'])
        );
        if out.len() + line.len() > room {
            let next = format!("{}:{}", cell_ref(r, c0), cell_ref(r1.min(rows - 1), c1));
            out.push_str(&format!(
                "[rows {}-{} not shown: read with sheet \"{}\" and range \"{next}\"]\n",
                r + 1,
                r1.min(rows - 1) + 1,
                s.name
            ));
            return Ok((out, Some(next)));
        }
        out.push_str(&line);
    }
    Ok((out, None))
}

fn sheet_page(s: &Sheet) -> Json {
    let (rows, cols) = s.size();
    let shown_rows = rows.min(PAGE_ROWS);
    let shown_cols = cols.min(PAGE_COLS);
    let mut out: Vec<Json> = Vec::with_capacity(shown_rows as usize);
    for r in 0..shown_rows {
        let mut row: Vec<Json> = Vec::new();
        for (&(_, c), cell) in s.cells.range((r, 0)..=(r, shown_cols.saturating_sub(1))) {
            while row.len() < c as usize {
                row.push(Json::Null);
            }
            let shown = cell.display();
            let input = cell.input();
            let mut v = json!({ "t": shown });
            if input != shown {
                v["e"] = json!(input);
            }
            match &cell.value {
                Value::Number(_) => v["n"] = json!(1),
                Value::Error(_) => v["x"] = json!(1),
                Value::Bool(_) => v["b"] = json!(1),
                _ => {}
            }
            if cell.formula.is_some() {
                v["f"] = json!(1);
            }
            row.push(v);
        }
        out.push(Json::Array(row));
    }
    json!({
        "name": s.name,
        "rows": out,
        "cols": shown_cols,
        "more_rows": rows - shown_rows,
        "more_cols": cols - shown_cols,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{apply_sheet, parse_markdown, Document, Edit, Workbook};

    fn file(content: Content) -> File {
        File {
            path: "x".into(),
            format: "test",
            version: "v".into(),
            size: 0,
            content,
        }
    }

    #[test]
    fn a_sheet_lists_with_addresses_and_formulas() {
        let mut book = Workbook {
            sheets: vec![Sheet::new("Sales")],
        };
        apply_sheet(
            &mut book,
            &Edit::parse_all(&serde_json::json!([{"cell": "A1", "rows": [["Region", "Q1"], ["North", 120], ["South", 80], ["Total", "=SUM(B2:B3)"]]}])).unwrap(),
        )
        .unwrap();
        let l = file(Content::Sheet(book.clone()))
            .listing(&Window::default())
            .unwrap();
        assert_eq!(
            l.text,
            "x: spreadsheet, 1 sheet: Sales (A1:B4)\n\nSales!A1:B4\n  | A | B\n1 | Region | Q1\n2 | North | 120\n3 | South | 80\n4 | Total | 200 [=SUM(B2:B3)]\n"
        );
        let part = file(Content::Sheet(book.clone()))
            .listing(&Window {
                range: Some("A3:B4".into()),
                ..Window::default()
            })
            .unwrap();
        assert!(
            part.text
                .contains("Sales!A3:B4\n  | A | B\n3 | South | 80\n4 | Total"),
            "{}",
            part.text
        );
        let page = file(Content::Sheet(book)).page();
        assert_eq!(
            page["sheets"][0]["rows"][3][1],
            serde_json::json!({"t": "200", "e": "=SUM(B2:B3)", "n": 1, "f": 1})
        );
    }

    #[test]
    fn a_document_lists_numbered_blocks_without_spacing() {
        let mut blocks = parse_markdown("# Title\nOne");
        blocks.insert(1, crate::model::Block::new(BlockKind::Paragraph, ""));
        let l = file(Content::Document(Document { blocks }))
            .listing(&Window::default())
            .unwrap();
        assert_eq!(l.text, "x: document, 2 blocks\n(the numbers are what files.edit takes)\n\n[1] # Title\n[3] One\n");
    }
}
