//! The Files tools: what an agent does with the project folder. Reads run freely; writing or
//! editing a file asks first. Every write names its file, so the window can open it, and is
//! stored as an artifact (a snapshot of what was written, kept with the call).

use std::path::Path;
use std::sync::Arc;

use ark_core::{ActionDescriptor, ArtifactKind, RiskTier};
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
                "read a file of the project: text, a spreadsheet (.xlsx, .csv) as rows with cell names, or a document (.docx) as numbered blocks",
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
            ActionDescriptor::new(SEARCH, "find text across the project's files, inside spreadsheets and documents too", RiskTier::Read)
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
            .with_input_schema(json!({
                "type": "object", "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "relative to the project root; the extension picks the format" },
                    "text": { "type": "string", "description": "the whole text; for .docx, Markdown: # headings, - and 1. lists, | tables, **bold**, *italic*" },
                    "rows": { "type": "array", "items": { "type": "array" }, "description": "spreadsheets with one sheet: rows of values; a value starting with = is a formula (=SUM(B2:B9))" },
                    "sheets": {
                        "type": "array", "description": "spreadsheets with several sheets",
                        "items": { "type": "object", "properties": { "name": { "type": "string" }, "rows": { "type": "array", "items": { "type": "array" } } } }
                    }
                }
            })),
            ActionDescriptor::new(
                EDIT,
                "change part of a file and keep the rest (its look, formulas, other sheets): set cells or add a sheet in a spreadsheet (.xlsx, .csv), replace or insert blocks in a document (.docx), find and replace text in any file",
                RiskTier::Mutate,
            )
            .with_input_schema(json!({
                "type": "object", "required": ["path", "edits"],
                "properties": {
                    "path": { "type": "string" },
                    "edits": {
                        "type": "array",
                        "description": "in order. Spreadsheets: {cell:\"B4\", value:150}, {cell:\"A10\", rows:[[\"Total\",\"=SUM(B2:B9)\"]]}, {sheet:\"Name\", ...} for another sheet, {clear:\"A2:C9\"}, {add_sheet:\"Summary\", rows:[[...]]}, {rename_sheet:\"Old\", to:\"New\"}, {delete_sheet:\"Name\"}. Documents, with the block numbers of the last read: {block:3, text:\"Markdown\"} replaces (empty text deletes), {after:3, text:\"Markdown\"} inserts (after:0 is the start), {delete:3}. Any file: {find:\"old\", replace:\"new\"}",
                        "items": { "type": "object" }
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
                walk(&root, &dir, 0, 2, &mut entries);
                let more = entries.len() > LIST_ENTRIES;
                entries.truncate(LIST_ENTRIES);
                Ok(ActionOutput::json(
                    json!({ "path": rel, "entries": entries, "more": more }),
                ))
            }
            READ => {
                let rel = s("path");
                let file = crate::open(&root, &rel).map_err(tool)?;
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
                Ok(ActionOutput::json(json!({
                    "path": file.path, "kind": file.kind().as_str(), "version": file.version,
                    "text": l.text, "more": l.more,
                })))
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
                let existed = crate::inside(&root, &rel)
                    .map(|p| p.exists())
                    .unwrap_or(false);
                let file = crate::write(&root, &rel, &content, None).map_err(tool)?;
                let what = format!(
                    "{} {} ({})",
                    if existed { "replaced" } else { "created" },
                    file.path,
                    size_words(&file)
                );
                Ok(saved(&root, file, what))
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
                    return Ok(saved(&root, file, what));
                }
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
                Ok(saved(&root, file, what))
            }
            other => Err(AdapterError::UnknownAction(other.into())),
        }
    }
}

/// A write's result: what changed, and the file as an artifact the window opens by its path.
fn saved(root: &Path, file: File, what: String) -> ActionOutput {
    let bytes = crate::inside(root, &file.path)
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .unwrap_or_default();
    let kind = match file.kind() {
        Kind::Text => ArtifactKind::Text,
        k => ArtifactKind::named(k.as_str()),
    };
    ActionOutput::json(json!({
        "path": file.path, "kind": file.kind().as_str(), "version": file.version, "summary": what,
    }))
    .with_command(vec!["write".into(), file.path.clone()])
    .with_artifact(
        ProducedArtifact::bytes(kind, file.name().to_string(), bytes).at(file.path.clone()),
    )
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
        Kind::Text => Err(Error::Input(format!(
            "there is no file {rel} yet; files.write creates it with its whole text"
        ))),
    }
}

/// The content a write asks for, in the shape the path's format takes.
fn content_from(rel: &str, input: &Json) -> std::result::Result<Content, Error> {
    let text = input.get("text").and_then(Json::as_str);
    match crate::kind_of(rel) {
        Kind::Text => Ok(Content::Text(
            text.ok_or_else(|| Error::Input("files.write needs `text` for this file".into()))?
                .to_string(),
        )),
        Kind::Document => Ok(Content::Document(Document {
            blocks: parse_markdown(
                text.ok_or_else(|| Error::Input("a document takes `text` as Markdown".into()))?,
            ),
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
    name.starts_with('.') || SKIP.contains(&name)
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
