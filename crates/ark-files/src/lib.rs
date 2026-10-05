//! # ark-files
//!
//! The files of a project folder as things an agent and a person work on together: text,
//! spreadsheets and documents. The file on disk is the one source of truth. The agent reads
//! and changes it through the [`Files`] tools; the window shows and edits the same file through
//! [`open`] and [`edit`]. Both send the same [`Edit`]s, so a change means one thing whoever
//! makes it, and each sees the other's changes on the next read.
//!
//! ```text
//! file (xlsx, csv, docx, text)  --Format-->  Content (Text | Sheet | Document)
//!                                               |  Edit (cells, blocks, find/replace)
//! agent: files.read / files.edit                |  window: file_read / file_edit
//! ```
//!
//! A [`Format`] translates one family of files to a shape and back, and edits a file in
//! place: what an edit does not touch keeps its bytes. Adding a format is one more `Format`
//! in [`FORMATS`]; one that maps to an existing shape needs no change anywhere else.

use std::path::{Component, Path, PathBuf};

mod format;
pub mod formula;
pub mod model;
mod package;
mod tool;
mod view;
mod xml;

pub use model::{
    Block, BlockKind, Cell, Content, Document, Edit, Fmt, Kind, Sheet, Value, Workbook,
};
pub use tool::{distill, Files, ACTIONS, EDIT, LIST, READ, SEARCH, WRITE};
pub use view::{Listing, Window, MAX_LISTING};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request does not fit the file (a wrong edit, a missing sheet). Says what would.
    #[error("{0}")]
    Input(String),
    /// The file cannot be read as its format says.
    #[error("{0}")]
    Format(String),
    /// The file changed since the version the change was based on.
    #[error("{0} changed since it was read; read it again")]
    Changed(String),
    #[error("{0}")]
    Path(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// One family of files: how its bytes read as a [`Content`], and how an edit or a new file
/// is written.
pub trait Format: Send + Sync {
    /// A short name (`xlsx`).
    fn name(&self) -> &'static str;
    /// The extensions it reads, lowercase, without the dot.
    fn extensions(&self) -> &'static [&'static str];
    fn kind(&self) -> Kind;
    fn read(&self, bytes: &[u8]) -> Result<Content, Error>;
    /// The file with `edits` applied; what they do not touch is kept.
    fn edit(&self, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, Error>;
    /// A new file with this content.
    fn create(&self, content: &Content) -> Result<Vec<u8>, Error>;
}

/// The formats ARK reads, by extension. Any other file that is text reads as text.
pub static FORMATS: &[&dyn Format] = &[&format::Xlsx, &format::Csv, &format::Docx];

/// Largest file ARK opens.
pub const MAX_BYTES: u64 = 25 * 1024 * 1024;

/// The format for a path, by its extension.
pub fn format_for(path: &str) -> Option<&'static dyn Format> {
    let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    FORMATS
        .iter()
        .copied()
        .find(|f| f.extensions().contains(&ext.as_str()))
}

/// Every extension ARK reads with a format, and the kind it reads as: what a front-end shows
/// before it opens a file.
pub fn extensions() -> Vec<(&'static str, Kind)> {
    FORMATS
        .iter()
        .flat_map(|f| f.extensions().iter().map(move |e| (*e, f.kind())))
        .collect()
}

/// What a path's extension says it holds; text when ARK has no format for it.
pub fn kind_of(path: &str) -> Kind {
    format_for(path).map_or(Kind::Text, |f| f.kind())
}

/// A file as read.
#[derive(Clone, Debug)]
pub struct File {
    /// Relative to the project folder, with `/`.
    pub path: String,
    /// The format's name (`xlsx`), or `text`.
    pub format: &'static str,
    /// A hash of the bytes: what an edit based on this read sends back.
    pub version: String,
    pub size: u64,
    pub content: Content,
}

impl File {
    pub fn kind(&self) -> Kind {
        self.content.kind()
    }
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

pub fn version_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..16].to_string()
}

/// `rel` inside `root`, or an error. `rel` may be empty (the root itself). A path that leaves
/// the folder (`..`, an absolute path, a link pointing out) is refused.
pub fn inside(root: &Path, rel: &str) -> Result<PathBuf, Error> {
    let rel = Path::new(rel.trim().trim_start_matches("./"));
    if rel
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::Path(format!(
            "`{}` is outside the project folder; use a path relative to it",
            rel.display()
        )));
    }
    let root = root.canonicalize()?;
    let path = root.join(rel);
    // A link may point out of the folder: check where it really is. A new file's nearest
    // existing folder decides.
    let mut probe = path.clone();
    let mut rest = Vec::new();
    let real = loop {
        match probe.canonicalize() {
            Ok(p) => {
                break rest
                    .iter()
                    .rev()
                    .fold(p, |acc: PathBuf, part| acc.join(part))
            }
            Err(_) => {
                let Some(name) = probe.file_name().map(|n| n.to_os_string()) else {
                    return Err(Error::Path(format!("no folder for `{}`", rel.display())));
                };
                rest.push(name);
                if !probe.pop() {
                    return Err(Error::Path(format!("no folder for `{}`", rel.display())));
                }
            }
        }
    };
    if !real.starts_with(&root) {
        return Err(Error::Path(format!(
            "`{}` is outside the project folder",
            rel.display()
        )));
    }
    Ok(real)
}

/// `p` relative to `root`, with `/`.
pub fn relative(root: &Path, p: &Path) -> String {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    p.strip_prefix(&root)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn read_bytes(path: &Path, rel: &str) -> Result<Vec<u8>, Error> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::Path(format!("there is no file {rel}")),
        _ => Error::Io(e),
    })?;
    if meta.is_dir() {
        return Err(Error::Path(format!("`{rel}` is a folder")));
    }
    if meta.len() > MAX_BYTES {
        return Err(Error::Format(format!(
            "{rel} is {} MB; ARK opens files up to {} MB",
            meta.len() / 1024 / 1024,
            MAX_BYTES / 1024 / 1024
        )));
    }
    Ok(std::fs::read(path)?)
}

fn decode(rel: &str, bytes: &[u8]) -> Result<(&'static str, Content), Error> {
    match format_for(rel) {
        Some(f) => f
            .read(bytes)
            .map(|c| (f.name(), c))
            .map_err(|e| Error::Format(format!("{rel} does not read as {}: {e}", f.name()))),
        None => {
            if bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
                return Err(Error::Format(format!(
                    "{rel} is not text or a format ARK reads"
                )));
            }
            format::Text.read(bytes).map(|c| ("text", c))
        }
    }
}

/// Read a file of the project folder.
pub fn open(root: &Path, rel: &str) -> Result<File, Error> {
    let path = inside(root, rel)?;
    let bytes = read_bytes(&path, rel)?;
    let (format, content) = decode(rel, &bytes)?;
    Ok(File {
        path: relative(root, &path),
        format,
        version: version_of(&bytes),
        size: bytes.len() as u64,
        content,
    })
}

/// The current version of a file, without reading it as its format.
pub fn version(root: &Path, rel: &str) -> Result<String, Error> {
    let path = inside(root, rel)?;
    Ok(version_of(&read_bytes(&path, rel)?))
}

/// Apply edits to a file and save it. With `base`, the edit is refused when the file is no
/// longer that version (someone else changed it since). Returns the file as saved.
pub fn edit(root: &Path, rel: &str, edits: &[Edit], base: Option<&str>) -> Result<File, Error> {
    if edits.is_empty() {
        return Err(Error::Input("there are no edits".into()));
    }
    let path = inside(root, rel)?;
    let shown = relative(root, &path);
    // Retried when the file changes between reading and writing it.
    for _ in 0..3 {
        let bytes = read_bytes(&path, rel)?;
        let current = version_of(&bytes);
        if base.is_some_and(|b| b != current) {
            return Err(Error::Changed(shown));
        }
        let new = match format_for(rel) {
            Some(f) => f.edit(&bytes, edits)?,
            None => format::Text.edit(&bytes, edits)?,
        };
        if version_of(&read_bytes(&path, rel)?) != current {
            if base.is_some() {
                return Err(Error::Changed(shown));
            }
            continue;
        }
        save(&path, &new)?;
        let (format, content) = decode(rel, &new)?;
        return Ok(File {
            path: shown,
            format,
            version: version_of(&new),
            size: new.len() as u64,
            content,
        });
    }
    Err(Error::Changed(shown))
}

/// Create a file, or replace one, with this content. With `base`, replacing is refused when
/// the file is no longer that version.
pub fn write(root: &Path, rel: &str, content: &Content, base: Option<&str>) -> Result<File, Error> {
    let path = inside(root, rel)?;
    let shown = relative(root, &path);
    if path.is_dir() {
        return Err(Error::Path(format!("`{rel}` is a folder")));
    }
    if let Some(b) = base {
        if path.exists() && version_of(&std::fs::read(&path)?) != b {
            return Err(Error::Changed(shown));
        }
    }
    let bytes = match format_for(rel) {
        Some(f) => f.create(content)?,
        None => format::Text.create(content)?,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    save(&path, &bytes)?;
    let (format, content) = decode(rel, &bytes)?;
    Ok(File {
        path: shown,
        format,
        version: version_of(&bytes),
        size: bytes.len() as u64,
        content,
    })
}

/// Write through a file beside the target, then rename: a reader never sees half a file.
fn save(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.ark-saving"));
    std::fs::write(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths_stay_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("notes")).unwrap();
        assert!(
            inside(root, "notes/new/deep.md").is_ok(),
            "a new file in a new folder"
        );
        for bad in ["../x", "/etc/passwd", "notes/../../x"] {
            assert!(inside(root, bad).is_err(), "{bad}");
        }
        #[cfg(unix)]
        {
            let out = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(out.path(), root.join("link")).unwrap();
            assert!(
                inside(root, "link/secret").is_err(),
                "a link out of the folder"
            );
            assert!(inside(root, "link/new/file.md").is_err());
        }
    }

    #[test]
    fn an_edit_based_on_an_old_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let rows = Content::Sheet(Workbook {
            sheets: vec![{
                let mut s = Sheet::new("Data");
                s.cells.insert(
                    (0, 0),
                    Cell {
                        value: Value::Number(1.0),
                        ..Cell::default()
                    },
                );
                s
            }],
        });
        let first = write(root, "data.csv", &rows, None).unwrap();
        let edits = Edit::parse_all(&json!([{"cell": "A1", "value": 2}])).unwrap();
        let second = edit(root, "data.csv", &edits, Some(&first.version)).unwrap();
        assert_ne!(first.version, second.version);
        let stale = edit(root, "data.csv", &edits, Some(&first.version)).unwrap_err();
        assert!(matches!(stale, Error::Changed(_)), "{stale}");
        assert_eq!(open(root, "data.csv").unwrap().version, second.version);
        assert!(!root.join(".data.csv.ark-saving").exists());
    }

    #[test]
    fn unknown_binary_files_are_not_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), [0u8, 1, 2]).unwrap();
        assert!(open(dir.path(), "a.bin").is_err());
        std::fs::write(dir.path().join("b.md"), "# hi").unwrap();
        assert_eq!(open(dir.path(), "b.md").unwrap().kind(), Kind::Text);
        assert_eq!(kind_of("x.XLSX"), Kind::Sheet);
    }
}
