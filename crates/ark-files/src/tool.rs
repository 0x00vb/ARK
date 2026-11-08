//! The Files tools: what an agent does with the project folder. Reads run freely; writing or
//! editing a file asks first. Every write names its file, so the window can open it, and is
//! stored as an artifact (a snapshot of what was written, kept with the call).

use std::path::Path;
use std::sync::Arc;

use ark_core::{ActionDescriptor, RiskTier};
use ark_tools::{ActionOutput, Adapter, AdapterError, ProducedArtifact, Result, RunCtx};
use async_trait::async_trait;
use serde_json::{json, Value as Json};

use crate::model::{
    apply_sheet, cell_ref, parse_markdown, parse_ref, Content, Document, Edit, Kind, Sheet,
    Workbook,
};
use crate::view::Window;
use crate::{Error, File};

pub const LIST: &str = "files.list";
pub const READ: &str = "files.read";
pub const SEARCH: &str = "files.search";
pub const WRITE: &str = "files.write";
pub const EDIT: &str = "files.edit";
pub const ACTIONS: &[&str] = &[LIST, READ, SEARCH, WRITE, EDIT];

/// Files larger than this are not searched.
const SEARCH_BYTES: u64 = 4 * 1024 * 1024;
const SEARCH_HITS: usize = 60;
const LIST_ENTRIES: usize = 400;
/// Folders a listing or search skips.
const SKIP: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "__pycache__",
    "dist",
    "build",
];

/// The tools. Register them on agents that work with the project's files.
pub struct Files;

fn path_schema(desc: &str) -> Json {
    json!({ "type": "object", "properties": { "path": { "type": "string", "description": desc } } })
}

/// Rows of cells: text, a number, true or false, or empty.
fn rows_schema(description: &str) -> Json {
    let mut s = json!({
        "type": "array",
        "items": { "type": "array", "items": { "type": ["string", "number", "boolean", "null"] } }
    });
    if !description.is_empty() {
        s["description"] = json!(description);
    }
    s
}

/// Each edit `files.edit` takes, as a closed object: the shapes `Edit::parse` reads. Closed,
/// a model's reply schema can hold them as objects (ADR 0049).
fn edit_schema() -> Json {
    let edit = |required: &[&str], props: Json| json!({ "type": "object", "required": required, "properties": props, "additionalProperties": false });
    let s = json!({ "type": "string" });
    let n = json!({ "type": "integer" });
    let f = json!({ "type": "number" });
    let cell_value = json!({ "type": ["string", "number", "boolean"] });
    json!({ "anyOf": [
        edit(&["find", "replace"], json!({ "find": s, "replace": s })),
        edit(&["cell", "value"], json!({ "cell": s, "value": cell_value, "sheet": s })),
        edit(&["cell", "rows"], json!({ "cell": s, "rows": rows_schema(""), "sheet": s })),
        edit(&["clear"], json!({ "clear": s, "sheet": s })),
        edit(&["add_sheet"], json!({ "add_sheet": s, "rows": rows_schema("") })),
        edit(&["rename_sheet", "to"], json!({ "rename_sheet": s, "to": s })),
        edit(&["delete_sheet"], json!({ "delete_sheet": s })),
        edit(&["block", "text"], json!({ "block": n, "text": s })),
        edit(&["after", "text"], json!({ "after": n, "text": s })),
        edit(&["delete"], json!({ "delete": n })),
        edit(&["format", "value"], json!({ "format": s, "value": f })),
    ] })
}

#[async_trait]
impl Adapter for Files {
    fn name(&self) -> &str {
        "files"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(LIST, "list the files under a folder of the project (default: its root)", RiskTier::Read)
                .with_input_schema(path_schema("a folder, relative to the project root")),
            ActionDescriptor::new(
                READ,
                "read a file of the project: text, a spreadsheet (.xlsx, .csv) as rows with cell names, a document (.docx) as numbered blocks, or the text of a PDF",
                RiskTier::Read,
            )
            .with_input_schema(json!({
                "type": "object", "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "relative to the project root" },
                    "sheet": { "type": "string", "description": "spreadsheets: one sheet" },
                    "range": { "type": "string", "description": "spreadsheets: cells like A1:F50" },
                    "from": { "type": "integer", "description": "documents: first block; text: first line" }
                }
            })),
            ActionDescriptor::new(SEARCH, "find text across the project's files, inside spreadsheets and documents too, and files and folders by name", RiskTier::Read)
                .with_input_schema(json!({
                    "type": "object", "required": ["query"],
                    "properties": {
                        "query": { "type": "string", "description": "text to find (case-insensitive)" },
                        "path": { "type": "string", "description": "a folder or file to search in" }
                    }
                })),
            ActionDescriptor::new(
                WRITE,
                "create or replace a whole file: text; a document (.docx) from Markdown; a spreadsheet (.xlsx, .csv) from rows",
                RiskTier::Mutate,
            )
            .auto_eligible()
            .with_input_schema(json!({
                "type": "object", "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "relative to the project root; the extension picks the format" },
                    "text": { "type": "string", "description": "the whole text; for .docx, Markdown: # headings, - and 1. lists, | tables, **bold**, *italic*" },
                    "rows": rows_schema("spreadsheets with one sheet: rows of values; a value starting with = is a formula (=SUM(B2:B9))"),
                    "sheets": {
                        "type": "array", "description": "spreadsheets with several sheets",
                        "items": { "type": "object", "properties": { "name": { "type": "string" }, "rows": rows_schema("") } }
                    }
                }
            })),
            ActionDescriptor::new(
                EDIT,
                "change part of a file and keep the rest (its look, formulas, other sheets): set cells or add a sheet in a spreadsheet (.xlsx, .csv), replace or insert blocks in a document (.docx), find and replace text in any file",
                RiskTier::Mutate,
            )
            .auto_eligible()
            .with_input_schema(json!({
                "type": "object", "required": ["path", "edits"],
                "properties": {
                    "path": { "type": "string" },
                    "edits": {
                        "type": "array",
                        "description": "Apply in order. Sheets: set {cell,value|rows}; optionally add, clear, rename or delete a sheet; use sheet to target one. Documents: {block,text}, {after,text}, {delete}; numbers come from read. Set a read layout field with {format,value}; spacing is points, margins inches. Any file: {find,replace}.",
                        "items": edit_schema()
                    },
                    "version": { "type": "string", "description": "the version the last read gave; the edit is refused if the file changed since" }
                }
            })),
        ]
    }

    async fn run(&self, action: &str, input: Json, ctx: &RunCtx<'_>) -> Result<ActionOutput> {
        let root = ctx
            .workspace
            .clone()
            .ok_or_else(|| AdapterError::Tool("this project has no folder".into()))?;
        let s = |k: &str| {
            input
                .get(k)
                .and_then(Json::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let tool = |e: Error| AdapterError::Tool(e.to_string());
        match action {
            LIST => {
                let rel = s("path");
                let dir = crate::inside(&root, &rel).map_err(tool)?;
                let mut entries = Vec::new();
                if dir.is_file() {
                    // A file given as the folder: it is there, and this is it (an empty
                    // listing made models conclude it did not exist).
                    let bytes = std::fs::metadata(&dir).map_or(0, |m| m.len());
                    entries.push(json!({ "path": crate::relative(&root, &dir), "bytes": bytes }));
                } else if !dir.exists() {
                    return Err(AdapterError::Tool(format!(
                        "nothing named `{rel}` in the project; list the root (no path) to see what it has"
                    )));
                }
                walk(&root, &dir, 0, 2, &mut entries);
                let more = entries.len() > LIST_ENTRIES;
                entries.truncate(LIST_ENTRIES);
                Ok(ActionOutput::json(
                    json!({ "path": rel, "entries": entries, "more": more }),
                ))
            }
            READ => {
                let rel = s("path");
                if let Some(why) = picture_over_cap(&root, &rel) {
                    let path = crate::inside(&root, &rel).map_err(tool)?;
                    return Ok(ActionOutput::json(json!({
                        "path": crate::relative(&root, &path), "kind": Kind::Image.as_str(),
                        "version": "", "text": why, "more": false,
                    })));
                }
                let (file, bytes) = crate::open_with_bytes(&root, &rel).map_err(tool)?;
                let w = Window {
                    sheet: Some(s("sheet")).filter(|x| !x.is_empty()),
                    range: Some(s("range")).filter(|x| !x.is_empty()),
                    from: input
                        .get("from")
                        .or_else(|| input.get("from_line"))
                        .and_then(Json::as_u64)
                        .map(|n| n as usize),
                };
                let l = file.listing(&w).map_err(tool)?;
                let mut text = l.text;
                let mut picture = None;
                if let Content::Media(m) = &file.content {
                    if m.media_type != "image/svg+xml" {
                        text.push_str("The picture is attached.\n");
                        picture = Some((m.media_type.clone(), bytes));
                    }
                }
                let out = ActionOutput::json(json!({
                    "path": file.path, "kind": file.kind().as_str(), "version": file.version,
                    "text": text, "more": l.more,
                }));
                Ok(match picture {
                    Some((media_type, bytes)) => out.with_image(&media_type, bytes),
                    None => out,
                })
            }
            SEARCH => {
                let query = s("query");
                if query.is_empty() {
                    return Err(AdapterError::Tool("files.search needs a query".into()));
                }
                let dir = crate::inside(&root, &s("path")).map_err(tool)?;
                let mut hits = Vec::new();
                search(&root, &dir, &query.to_lowercase(), &mut hits);
                let more = hits.len() > SEARCH_HITS;
                hits.truncate(SEARCH_HITS);
                Ok(ActionOutput::json(
                    json!({ "query": query, "hits": hits, "more": more }),
                ))
            }
            WRITE => {
                let rel = s("path");
                if rel.is_empty() {
                    return Err(AdapterError::Tool("files.write needs a path".into()));
                }
                let content = content_from(&rel, &input).map_err(tool)?;
                let was = read_before(&root, &rel);
                let file = crate::write(&root, &rel, &content, None).map_err(tool)?;
                let what = format!(
                    "{} {} ({})",
                    if was.is_some() { "replaced" } else { "created" },
                    file.path,
                    size_words(&file)
                );
                Ok(saved(&root, file, what, was))
            }
            EDIT => {
                let rel = s("path");
                let edits =
                    Edit::parse_all(input.get("edits").unwrap_or(&Json::Null)).map_err(tool)?;
                let base = Some(s("version")).filter(|v| !v.is_empty());
                if !crate::inside(&root, &rel)
                    .map(|p| p.exists())
                    .unwrap_or(true)
                {
                    // Edits to a spreadsheet or document that does not exist yet make it.
                    let content = created(&rel, &edits).map_err(tool)?;
                    let file = crate::write(&root, &rel, &content, None).map_err(tool)?;
                    let what = format!("created {} ({})", file.path, size_words(&file));
                    return Ok(saved(&root, file, what, None));
                }
                let was = read_before(&root, &rel);
                let file = crate::edit(&root, &rel, &edits, base.as_deref()).map_err(tool)?;
                let mut what = format!(
                    "saved {}: {}",
                    file.path,
                    edits
                        .iter()
                        .map(Edit::describe)
                        .collect::<Vec<_>>()
                        .join("; ")
                );
                if let Some(r) = results(&file, &edits) {
                    what.push_str(&format!(". Now {r}"));
                }
                if file.kind() == Kind::Document
                    && edits.iter().any(|e| !matches!(e, Edit::FindReplace { .. }))
                {
                    what.push_str(". Block numbers moved: read again before editing by number");
                }
                Ok(saved(&root, file, what, was))
            }
            other => Err(AdapterError::UnknownAction(other.into())),
        }
    }
}

/// The file at `rel` as it is, before a write changes it, or `None` when there is none.
fn read_before(root: &Path, rel: &str) -> Option<Vec<u8>> {
    crate::inside(root, rel)
        .ok()
        .and_then(|p| std::fs::read(p).ok())
}

/// A write's result: what changed, and the file as an artifact the window opens by its path.
/// `was`, the file before the write, is kept too when the write changed it (ADR 0034).
fn saved(root: &Path, file: File, what: String, was: Option<Vec<u8>>) -> ActionOutput {
    let bytes = crate::inside(root, &file.path)
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .unwrap_or_default();
    let kind = crate::artifact_kind(file.kind());
    let was = was.filter(|w| *w != bytes);
    let mut out = ActionOutput::json(json!({
        "path": file.path, "kind": file.kind().as_str(), "version": file.version, "summary": what,
    }))
    .with_command(vec!["write".into(), file.path.clone()])
    .with_artifact(
        ProducedArtifact::bytes(kind.clone(), file.name().to_string(), bytes).at(file.path.clone()),
    );
    if let Some(was) = was {
        out = out.with_artifact(ProducedArtifact::before(kind, file.path.clone(), was));
    }
    out
}

fn size_words(f: &File) -> String {
    match &f.content {
        Content::Text(t) => format!("{} lines", t.lines().count()),
        Content::Sheet(b) => b
            .sheets
            .iter()
            .map(|s| format!("{} {}", s.name, s.range()))
            .collect::<Vec<_>>()
            .join(", "),
        Content::Document(d) => format!(
            "{} blocks",
            d.blocks.iter().filter(|b| !b.is_blank()).count()
        ),
        Content::Media(m) => m.media_type.clone(),
    }
}

/// The values of formula cells an edit wrote, as the file now has them.
fn results(f: &File, edits: &[Edit]) -> Option<String> {
    let Content::Sheet(book) = &f.content else {
        return None;
    };
    let mut out = Vec::new();
    for e in edits {
        let (sheet, at, rows) = match e {
            Edit::Set { sheet, at, rows } => (sheet.clone(), at.as_str(), rows),
            Edit::AddSheet { name, rows } => (Some(name.clone()), "A1", rows),
            _ => continue,
        };
        let s = match &sheet {
            Some(n) => book.sheets.iter().find(|s| s.name.eq_ignore_ascii_case(n)),
            None => book.sheets.first(),
        }?;
        let (r0, c0) = parse_ref(at)?;
        for (dr, row) in rows.iter().enumerate() {
            for dc in 0..row.len() {
                let (r, c) = (r0 + dr as u32, c0 + dc as u32);
                if let Some(cell) = s.get(r, c).filter(|c| c.formula.is_some()) {
                    out.push(format!(
                        "{}!{} = {}",
                        s.name,
                        cell_ref(r, c),
                        cell.display()
                    ));
                }
            }
        }
    }
    (!out.is_empty()).then(|| out.into_iter().take(20).collect::<Vec<_>>().join(", "))
}

/// A new file from edits sent to a path with no file yet: the blocks a document's edits
/// insert, in order; a workbook the cell edits fill.
fn created(rel: &str, edits: &[Edit]) -> std::result::Result<Content, Error> {
    match crate::kind_of(rel) {
        Kind::Document => {
            let text: Vec<&str> = edits
                .iter()
                .filter_map(|e| match e {
                    Edit::Replace { text, .. } | Edit::Insert { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if text.is_empty() {
                return Err(Error::Input(format!(
                    "there is no file {rel} yet; files.write creates it from Markdown"
                )));
            }
            Ok(Content::Document(Document {
                blocks: parse_markdown(&text.join("\n")),
                ..Document::default()
            }))
        }
        Kind::Sheet => {
            let mut book = Workbook::default();
            if !edits.iter().any(|e| matches!(e, Edit::AddSheet { .. }))
                || edits
                    .iter()
                    .any(|e| matches!(e, Edit::Set { sheet: None, .. }))
            {
                book.sheets.push(Sheet::new("Sheet1"));
            }
            apply_sheet(&mut book, edits)?;
            Ok(Content::Sheet(book))
        }
        Kind::Text | Kind::Markdown | Kind::Map => Err(Error::Input(format!(
            "there is no file {rel} yet; files.write creates it with its whole text"
        ))),
        Kind::Pdf | Kind::Image | Kind::Model => Err(not_written(rel)),
    }
}

fn not_written(rel: &str) -> Error {
    Error::Input(format!(
        "ARK reads {rel} but does not write PDFs or images with files.write; a program run with code.run can make one"
    ))
}

/// The content a write asks for, in the shape the path's format takes.
fn content_from(rel: &str, input: &Json) -> std::result::Result<Content, Error> {
    let text = input.get("text").and_then(Json::as_str);
    match crate::kind_of(rel) {
        Kind::Pdf | Kind::Image | Kind::Model => Err(not_written(rel)),
        Kind::Text | Kind::Markdown | Kind::Map => Ok(Content::Text(
            text.ok_or_else(|| Error::Input("files.write needs `text` for this file".into()))?
                .to_string(),
        )),
        Kind::Document => Ok(Content::Document(Document {
            blocks: parse_markdown(
                text.ok_or_else(|| Error::Input("a document takes `text` as Markdown".into()))?,
            ),
            ..Document::default()
        })),
        Kind::Sheet => {
            let mut book = Workbook::default();
            let sheets: Vec<(String, Json)> = match (input.get("sheets"), input.get("rows"), text) {
                (Some(Json::Array(list)), _, _) => list
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let name = s
                            .get("name")
                            .and_then(Json::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("Sheet{}", i + 1));
                        (
                            name,
                            s.get("rows").cloned().unwrap_or(Json::Array(Vec::new())),
                        )
                    })
                    .collect(),
                (_, Some(rows), _) => vec![("Sheet1".into(), rows.clone())],
                (_, _, Some(t)) => {
                    // CSV text works for one sheet.
                    let sheet = crate::format::csv_sheet(t);
                    book.sheets.push(sheet);
                    return Ok(Content::Sheet(book));
                }
                _ => {
                    return Err(Error::Input(
                        "a spreadsheet takes `rows` (one sheet) or `sheets` [{name, rows}]".into(),
                    ))
                }
            };
            for (name, rows) in sheets {
                book.sheets.push(Sheet::new(name.clone()));
                let edit = Edit::parse(&json!({ "sheet": name, "cell": "A1", "rows": rows }));
                // No rows: an empty sheet.
                if let Ok(e) = edit {
                    apply_sheet(&mut book, &[e])?;
                }
            }
            Ok(Content::Sheet(book))
        }
    }
}

// ---- list and search ---------------------------------------------------------------------------

fn skipped(name: &str) -> bool {
    name.starts_with('.') || name.starts_with("~$") || SKIP.contains(&name)
}

fn walk(root: &Path, dir: &Path, depth: usize, max: usize, out: &mut Vec<Json>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = rd.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for e in items {
        if out.len() > LIST_ENTRIES {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if skipped(&name) {
            continue;
        }
        let p = e.path();
        let rel = crate::relative(root, &p);
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            out.push(json!({ "path": format!("{rel}/"), "dir": true }));
            if depth + 1 < max {
                walk(root, &p, depth + 1, max, out);
            }
        } else {
            out.push(json!({ "path": rel, "bytes": meta.len() }));
        }
    }
}

/// Lines (cells, blocks) containing `needle` (lowercase) under `dir`, or in `dir` when it is a
/// file.
fn search(root: &Path, dir: &Path, needle: &str, hits: &mut Vec<String>) {
    if dir.is_file() {
        search_file(root, dir, needle, hits);
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = rd.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for e in items {
        if hits.len() > SEARCH_HITS {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if skipped(&name) {
            continue;
        }
        let p = e.path();
        let Ok(meta) = e.metadata() else { continue };
        // A name matches too: people look for a file by the name they know.
        if name.to_lowercase().contains(needle) {
            let rel = crate::relative(root, &p);
            hits.push(if meta.is_dir() {
                format!("{rel}/: folder name")
            } else {
                format!("{rel}: file name")
            });
        }
        if meta.is_dir() {
            search(root, &p, needle, hits);
        } else {
            search_file(root, &p, needle, hits);
        }
    }
}

fn search_file(root: &Path, p: &Path, needle: &str, hits: &mut Vec<String>) {
    if std::fs::metadata(p).map_or(true, |m| m.len() > SEARCH_BYTES) {
        return;
    }
    let rel = crate::relative(root, p);
    let clip = |s: &str| s.trim().chars().take(200).collect::<String>();
    let push = |hits: &mut Vec<String>, at: String, text: &str| {
        if hits.len() <= SEARCH_HITS {
            hits.push(format!("{rel}:{at}: {}", clip(text)));
        }
    };
    match crate::format_for(&rel) {
        Some(f) => {
            let Ok(content) = std::fs::read(p)
                .map_err(|_| ())
                .and_then(|b| f.read(&b).map_err(|_| ()))
            else {
                return;
            };
            match content {
                Content::Sheet(book) => {
                    for s in &book.sheets {
                        for (&(r, c), cell) in &s.cells {
                            let t = cell.display();
                            if t.to_lowercase().contains(needle) {
                                push(hits, format!("{}!{}", s.name, cell_ref(r, c)), &t);
                            }
                        }
                        if s.unread > 0 {
                            push(
                                hits,
                                s.name.clone(),
                                &format!(
                                    "its last {} rows not searched (a large sheet; a program reads all of it)",
                                    s.unread
                                ),
                            );
                        }
                    }
                }
                Content::Document(doc) => {
                    for (i, b) in doc.blocks.iter().enumerate() {
                        let md = b.to_markdown();
                        if md.to_lowercase().contains(needle) {
                            push(hits, format!("block {}", i + 1), &md);
                        }
                    }
                }
                Content::Media(_) => {}
                Content::Text(t) => {
                    for (i, line) in t.lines().enumerate() {
                        if line.to_lowercase().contains(needle) {
                            push(hits, (i + 1).to_string(), line);
                        }
                    }
                }
            }
        }
        None => {
            let Ok(text) = std::fs::read_to_string(p) else {
                return;
            };
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(needle) {
                    push(hits, (i + 1).to_string(), line);
                    if hits.len() > SEARCH_HITS {
                        return;
                    }
                }
            }
        }
    }
}

/// A result summarizer, the shape `ark_runtime::Distiller` has.
pub type Distiller = Arc<dyn Fn(&str, &Json) -> Option<(String, Vec<String>)> + Send + Sync>;

/// What the model sees of each result: the file's text and the outcome, not a JSON dump. Add
/// it to the profile's distillers when an app registers [`Files`] under another name; ARK's
/// own distiller already reads these results.
pub fn distill() -> Distiller {
    Arc::new(|action: &str, out: &Json| {
        let s = |k: &str| out.get(k).and_then(Json::as_str).unwrap_or("").to_string();
        match action {
            READ => Some((
                format!("{}\nversion {}", s("text").trim_end(), s("version")),
                vec![s("path")],
            )),
            WRITE | EDIT => Some((
                format!("{} (version {})", s("summary"), s("version")),
                vec![s("path")],
            )),
            SEARCH => {
                let hits: Vec<&str> = out["hits"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Json::as_str).collect())
                    .unwrap_or_default();
                let mut t = if hits.is_empty() {
                    format!("nothing contains \"{}\"", s("query"))
                } else {
                    hits.join("\n")
                };
                if out["more"] == true {
                    t.push_str("\n[more matches: narrow the query or the folder]");
                }
                Some((t, Vec::new()))
            }
            LIST => {
                let paths: Vec<&str> = out["entries"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|e| e["path"].as_str()).collect())
                    .unwrap_or_default();
                Some((
                    if paths.is_empty() {
                        "(empty folder)".into()
                    } else {
                        paths.join("\n")
                    },
                    Vec::new(),
                ))
            }
            _ => None,
        }
    })
}

const PICTURE_CAP: u64 = 3_670_016;

/// Why a picture is not attached, from the file's metadata alone, so an over-cap file is never
/// read. `None` for anything that is not a picture the model could look at, or fits the cap.
fn picture_over_cap(root: &Path, rel: &str) -> Option<String> {
    if crate::kind_of(rel) != Kind::Image || rel.to_lowercase().ends_with(".svg") {
        return None;
    }
    let size = std::fs::metadata(crate::inside(root, rel).ok()?)
        .ok()?
        .len();
    (size > PICTURE_CAP).then(|| {
        format!(
            "The picture is not attached: it is {:.1} MB and the limit is {:.1} MB.\n",
            size as f64 / 1_048_576.0,
            PICTURE_CAP as f64 / 1_048_576.0
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::ArtifactId;
    use std::path::PathBuf;

    struct NoBlobs;
    impl ark_tools::BlobResolver for NoBlobs {
        fn path(&self, _: &ArtifactId) -> Option<PathBuf> {
            None
        }
        fn read(&self, id: &ArtifactId) -> Result<Vec<u8>> {
            Err(AdapterError::ArtifactNotFound(id.to_string()))
        }
    }

    async fn run(
        root: &Path,
        action: &str,
        input: Json,
    ) -> std::result::Result<ActionOutput, String> {
        let blobs = NoBlobs;
        let ctx = RunCtx::new(&blobs).with_workspace(root.to_path_buf());
        Files
            .run(action, input, &ctx)
            .await
            .map_err(|e| e.to_string())
    }

    fn said(out: &ActionOutput, action: &str) -> String {
        distill()(action, &out.output).unwrap().0
    }

    fn png_header(pad: usize) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&3u32.to_be_bytes());
        png.extend_from_slice(&2u32.to_be_bytes());
        png.resize(png.len() + pad, 0);
        png
    }

    #[tokio::test]
    async fn reading_a_png_attaches_the_picture() {
        let dir = tempfile::tempdir().unwrap();
        let png = png_header(16);
        std::fs::write(dir.path().join("shot.png"), &png).unwrap();
        let out = run(dir.path(), READ, json!({"path": "shot.png"}))
            .await
            .unwrap();
        assert_eq!(out.pictures.len(), 1);
        assert_eq!(out.pictures[0].media_type, "image/png");
        assert_eq!(*out.pictures[0].bytes, png);
        let text = said(&out, READ);
        assert!(text.contains("The picture is attached."), "{text}");
        assert!(!text.contains("not readable"), "{text}");
    }

    #[tokio::test]
    async fn a_picture_over_the_cap_is_not_attached_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big.png"), png_header(6 * 1024 * 1024)).unwrap();
        let out = run(dir.path(), READ, json!({"path": "big.png"}))
            .await
            .unwrap();
        assert!(out.pictures.is_empty());
        let text = said(&out, READ);
        assert!(
            text.contains("not attached: it is 6.0 MB and the limit is 3.5 MB"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn an_over_cap_picture_is_refused_before_any_read() {
        let dir = tempfile::tempdir().unwrap();
        let big = std::fs::File::create(dir.path().join("huge.png")).unwrap();
        big.set_len(30 * 1024 * 1024).unwrap();
        let out = run(dir.path(), READ, json!({"path": "huge.png"}))
            .await
            .unwrap();
        assert!(out.pictures.is_empty());
        let text = said(&out, READ);
        assert!(text.contains("not attached: it is 30.0 MB"), "{text}");
    }

    #[tokio::test]
    async fn a_picture_of_exactly_the_cap_is_attached() {
        let dir = tempfile::tempdir().unwrap();
        let png = png_header(PICTURE_CAP as usize - png_header(0).len());
        assert_eq!(png.len() as u64, PICTURE_CAP);
        std::fs::write(dir.path().join("edge.png"), &png).unwrap();
        let out = run(dir.path(), READ, json!({"path": "edge.png"}))
            .await
            .unwrap();
        assert_eq!(out.pictures.len(), 1);
    }

    #[tokio::test]
    async fn jpeg_and_webp_get_their_media_type() {
        let dir = tempfile::tempdir().unwrap();
        let jpeg = b"\xff\xd8\xff\xe0\0\x10JFIF\0\x01\x01\0\0\x01\0\x01\0\0".to_vec();
        let mut webp = b"RIFF\x10\0\0\0WEBPVP8 ".to_vec();
        webp.resize(32, 0);
        for (name, bytes, media) in [
            ("a.jpg", jpeg, "image/jpeg"),
            ("b.webp", webp, "image/webp"),
        ] {
            std::fs::write(dir.path().join(name), &bytes).unwrap();
            let out = run(dir.path(), READ, json!({"path": name})).await.unwrap();
            assert_eq!(out.pictures.len(), 1, "{name}");
            assert_eq!(out.pictures[0].media_type, media);
            assert_eq!(*out.pictures[0].bytes, bytes);
        }
    }

    #[tokio::test]
    async fn an_svg_stays_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.svg"), "<svg xmlns='x'></svg>").unwrap();
        let out = run(dir.path(), READ, json!({"path": "a.svg"}))
            .await
            .unwrap();
        assert!(out.pictures.is_empty());
    }

    /// The loop the feature is for: the agent makes a workbook, a person changes a cell in the
    /// window, the agent reads the person's version and builds on it.
    #[tokio::test]
    async fn agent_and_person_work_on_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let out = run(root, WRITE, json!({"path": "sales.xlsx", "sheets": [{"name": "Sales", "rows": [["Region", "Q1"], ["North", 120], ["South", 80]]}]})).await.unwrap();
        assert_eq!(
            said(&out, WRITE).split(" (version").next().unwrap(),
            "created sales.xlsx (Sales A1:B3)"
        );
        assert_eq!(out.produced[0].path.as_deref(), Some("sales.xlsx"));
        assert_eq!(out.produced[0].kind.as_tag(), "sheet");

        // The person edits B3 in the window: the same edit, the same file.
        let seen = crate::open(root, "sales.xlsx").unwrap();
        crate::edit(
            root,
            "sales.xlsx",
            &Edit::parse_all(&json!([{"cell": "B3", "value": 95}])).unwrap(),
            Some(&seen.version),
        )
        .unwrap();

        let read = run(root, READ, json!({"path": "sales.xlsx"}))
            .await
            .unwrap();
        assert!(
            said(&read, READ).contains("3 | South | 95"),
            "{}",
            said(&read, READ)
        );
        let out = run(root, EDIT, json!({"path": "sales.xlsx", "edits": [{"add_sheet": "Summary", "rows": [["Total", "=SUM(Sales!B2:B3)"]]}]})).await.unwrap();
        assert!(
            said(&out, EDIT).contains("Now Summary!B1 = 215"),
            "{}",
            said(&out, EDIT)
        );

        // An edit based on what the agent read before the person's change is refused.
        let stale = run(root, EDIT, json!({"path": "sales.xlsx", "version": seen.version, "edits": [{"cell": "B2", "value": 1}]})).await.err().unwrap();
        assert!(stale.contains("changed since it was read"), "{stale}");
    }

    /// ADR 0034: a save over a file keeps the file as it was; a new file or a save that changes
    /// nothing keeps no version.
    #[tokio::test]
    async fn a_save_over_a_file_keeps_the_version_before() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let before = |out: &ActionOutput| -> Vec<(String, Vec<u8>)> {
            out.produced
                .iter()
                .filter(|a| a.before)
                .map(|a| (a.path.clone().unwrap(), a.bytes.clone()))
                .collect()
        };
        let made = run(root, WRITE, json!({"path": "notes.md", "text": "one"}))
            .await
            .unwrap();
        assert!(before(&made).is_empty());
        let first = std::fs::read(root.join("notes.md")).unwrap();

        let out = run(root, WRITE, json!({"path": "notes.md", "text": "two"}))
            .await
            .unwrap();
        assert_eq!(before(&out), vec![("notes.md".to_string(), first)]);
        assert_eq!(out.produced[0].path.as_deref(), Some("notes.md"));
        assert!(!out.produced[0].before);

        let second = std::fs::read(root.join("notes.md")).unwrap();
        let out = run(
            root,
            EDIT,
            json!({"path": "notes.md", "edits": [{"find": "two", "replace": "three"}]}),
        )
        .await
        .unwrap();
        assert_eq!(before(&out), vec![("notes.md".to_string(), second)]);

        let out = run(root, WRITE, json!({"path": "notes.md", "text": "three"}))
            .await
            .unwrap();
        assert!(before(&out).is_empty(), "nothing changed");
    }

    /// What a small model tried when looking for a file it was told about (the `files-edit`
    /// workload): listing the file itself, then searching for its name.
    #[tokio::test]
    async fn a_file_is_found_by_listing_it_or_searching_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(
            root,
            WRITE,
            json!({"path": "sales.xlsx", "rows": [["Month"]]}),
        )
        .await
        .unwrap();
        let listed = run(root, LIST, json!({"path": "sales.xlsx"}))
            .await
            .unwrap();
        assert_eq!(said(&listed, LIST), "sales.xlsx");
        let missing = run(root, LIST, json!({"path": "sales.csv"}))
            .await
            .err()
            .unwrap();
        assert!(missing.contains("nothing named `sales.csv`"), "{missing}");
        let hits = run(root, SEARCH, json!({"query": "sales.xlsx"}))
            .await
            .unwrap();
        assert_eq!(said(&hits, SEARCH), "sales.xlsx: file name");
    }

    #[tokio::test]
    async fn office_lock_files_are_hidden_from_discovery_but_explicit_paths_work() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(
            root,
            WRITE,
            json!({"path": "~$book.xlsx", "rows": [["hidden workbook marker"]]}),
        )
        .await
        .unwrap();
        run(
            root,
            WRITE,
            json!({"path": "book.xlsx", "rows": [["visible workbook marker"]]}),
        )
        .await
        .unwrap();
        std::fs::write(root.join("~$notes.txt"), "hidden note marker").unwrap();
        std::fs::write(root.join("notes.txt"), "visible note marker").unwrap();

        let listed = run(root, LIST, json!({})).await.unwrap();
        let listing = said(&listed, LIST);
        assert!(!listing.contains("~$book.xlsx"), "{listing}");
        assert!(!listing.contains("~$notes.txt"), "{listing}");
        assert!(listing.contains("book.xlsx"), "{listing}");
        assert!(listing.contains("notes.txt"), "{listing}");

        let hidden_name = run(root, SEARCH, json!({"query": "~$book.xlsx"}))
            .await
            .unwrap();
        assert!(hidden_name.output["hits"].as_array().unwrap().is_empty());
        let visible_name = run(root, SEARCH, json!({"query": "book.xlsx"}))
            .await
            .unwrap();
        assert!(said(&visible_name, SEARCH).contains("book.xlsx: file name"));
        let hidden_content = run(root, SEARCH, json!({"query": "hidden note marker"}))
            .await
            .unwrap();
        assert!(hidden_content.output["hits"].as_array().unwrap().is_empty());
        let visible_content = run(root, SEARCH, json!({"query": "visible note marker"}))
            .await
            .unwrap();
        assert!(said(&visible_content, SEARCH).contains("notes.txt"));

        let explicit_read = run(root, READ, json!({"path": "~$book.xlsx"}))
            .await
            .unwrap();
        assert!(said(&explicit_read, READ).contains("hidden workbook marker"));
        let explicit_search = run(
            root,
            SEARCH,
            json!({"path": "~$notes.txt", "query": "hidden note marker"}),
        )
        .await
        .unwrap();
        assert!(said(&explicit_search, SEARCH).contains("hidden note marker"));
    }

    #[tokio::test]
    async fn documents_from_markdown_and_search_inside_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(
            root,
            WRITE,
            json!({"path": "report.docx", "text": "# Q1\n\nSales rose in the north."}),
        )
        .await
        .unwrap();
        run(
            root,
            WRITE,
            json!({"path": "data.csv", "rows": [["Region"], ["North"]]}),
        )
        .await
        .unwrap();
        std::fs::write(root.join("notes.md"), "north star\n").unwrap();
        let out = run(
            root,
            EDIT,
            json!({"path": "report.docx", "edits": [{"after": 2, "text": "- North: +12%"}]}),
        )
        .await
        .unwrap();
        assert!(said(&out, EDIT).contains("read again before editing by number"));
        let read = run(root, READ, json!({"path": "report.docx"}))
            .await
            .unwrap();
        assert!(
            said(&read, READ).contains("[3] - North: +12%"),
            "{}",
            said(&read, READ)
        );
        let hits = run(root, SEARCH, json!({"query": "NORTH"})).await.unwrap();
        assert_eq!(
            said(&hits, SEARCH),
            "data.csv:Sheet1!A2: North\nnotes.md:1: north star\nreport.docx:block 2: Sales rose in the north.\nreport.docx:block 3: - North: +12%"
        );
        let e = run(
            root,
            EDIT,
            json!({"path": "report.docx", "edits": [{"cell": "A1", "value": 1}]}),
        )
        .await
        .err()
        .unwrap();
        // Edits to a file that is not there yet make it.
        let out = run(
            root,
            EDIT,
            json!({"path": "new.docx", "edits": [{"block": 0, "text": "# New\n\nFirst line."}]}),
        )
        .await
        .unwrap();
        assert!(
            said(&out, EDIT).starts_with("created new.docx (2 blocks)"),
            "{}",
            said(&out, EDIT)
        );
        let out = run(root, EDIT, json!({"path": "new.xlsx", "edits": [{"cell": "A1", "rows": [["a", 1], ["b", "=B1*2"]]}]})).await.unwrap();
        assert!(
            said(&out, EDIT).starts_with("created new.xlsx (Sheet1 A1:B2)"),
            "{}",
            said(&out, EDIT)
        );
        assert!(run(
            root,
            EDIT,
            json!({"path": "new.md", "edits": [{"find": "a", "replace": "b"}]})
        )
        .await
        .is_err());
        assert!(e.contains("a document takes"), "{e}");
        assert!(run(root, READ, json!({"path": "../x"})).await.is_err());
    }
}
