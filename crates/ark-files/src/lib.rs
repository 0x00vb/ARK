//! # ark-files
//!
//! The files of a project folder as things an agent and a person work on together: text,
//! Markdown, spreadsheets, documents, PDFs and images. The file on disk is the one source of truth. The agent reads
//! and changes it through the [`Files`] tools; the window shows and edits the same file through
//! [`open`] and [`edit`]. Both send the same [`Edit`]s, so a change means one thing whoever
//! makes it, and each sees the other's changes on the next read.
//!
//! ```text
//! file (xlsx, csv, docx, md, pdf, png, text)  --Format-->  Content (Text | Sheet | Document | Media)
//!                                                             |  Edit (cells, blocks, find/replace)
//! agent: files.read / files.edit                              |  window: file_read / file_edit
//! ```
//!
//! A [`Format`] translates one family of files to a shape and back, and edits a file in
//! place: what an edit does not touch keeps its bytes. Its [`Kind`] is what the person sees
//! and the key of the window's view for it; a PDF reads as a document but shows as a PDF.
//! Adding a format is one more `Format` in [`FORMATS`]; one that maps to an existing kind
//! needs no change anywhere else. The kind of any file, whichever tool wrote it, is decided
//! here once: [`kind_of`] for a path, [`Kinds`] for a stored artifact.

use std::path::{Component, Path, PathBuf};

mod format;
pub mod formula;
mod images;
pub mod model;
mod package;
pub mod snapshot;
mod tool;
mod view;
mod xml;

pub use images::{images, AttachedImage, MAX_IMAGES, MAX_IMAGES_BYTES, MAX_IMAGE_BYTES};
pub use model::{
    Block, BlockKind, Cell, Content, Document, Edit, Fmt, Kind, Media, Sheet, Value, Workbook,
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
    #[error("{}", io_error_message(.0, cfg!(windows)))]
    Io(#[from] std::io::Error),
}

fn io_error_message(error: &std::io::Error, windows: bool) -> String {
    if windows && matches!(error.raw_os_error(), Some(32 | 33)) {
        format!("file is in use by another application; close it there and try again: {error}")
    } else {
        error.to_string()
    }
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
pub static FORMATS: &[&dyn Format] = &[
    &format::Xlsx,
    &format::Csv,
    &format::Docx,
    &format::Markdown,
    &format::Pdf,
    &format::Image,
    &format::GeoJson,
];

/// Largest file ARK opens.
pub const MAX_BYTES: u64 = 25 * 1024 * 1024;

/// Cells of one sheet a read keeps to show it (the agent reads about 12,000 characters at a
/// time, the window shows 5,000 rows). The rows after are counted, not kept ([`Sheet::unread`]),
/// so a large workbook opens in a fraction of the memory. An edit reads the whole file.
pub const READ_CELLS: usize = 100_000;

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
        .chain(VIEW_ONLY.iter().map(|v| (v.extension, v.kind)))
        .collect()
}

/// Opaque file families. A descriptor never decodes a model or grants editing authority.
pub struct ViewOnly {
    pub extension: &'static str,
    pub kind: Kind,
    pub media_type: &'static str,
}

impl ViewOnly {
    /// Whether the opaque format also has readable, not editable, UTF-8 source.
    pub fn has_text_source(&self) -> bool {
        self.extension == "ifc"
    }
    /// Maximum source bytes before a host copies a snapshot. Decoded limits belong to viewers.
    pub fn max_bytes(&self) -> u64 {
        if self.extension == "ifc" {
            2 * 1024 * 1024
        } else {
            64 * 1024 * 1024
        }
    }
    /// Only the header is inspected here. The consumer's worker validates decoded geometry.
    pub fn preflight(&self, head: &[u8], size: u64) -> Result<(), Error> {
        let valid = match self.extension {
            "ifc" => head.starts_with(b"ISO-10303-21;"),
            "glb" => {
                head.len() >= 12
                    && &head[..4] == b"glTF"
                    && u32::from_le_bytes(head[4..8].try_into().unwrap()) == 2
                    && u32::from_le_bytes(head[8..12].try_into().unwrap()) as u64 == size
            }
            "stl" => {
                (head.len() >= 84
                    && 84 + u32::from_le_bytes(head[80..84].try_into().unwrap()) as u64 * 50
                        == size)
                    || head.starts_with(b"solid")
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Format(format!(
                "This {} file has an invalid header",
                self.extension.to_uppercase()
            )))
        }
    }
}

pub static VIEW_ONLY: &[ViewOnly] = &[
    ViewOnly {
        extension: "ifc",
        kind: Kind::Model,
        media_type: "application/x-step",
    },
    ViewOnly {
        extension: "glb",
        kind: Kind::Model,
        media_type: "model/gltf-binary",
    },
    ViewOnly {
        extension: "stl",
        kind: Kind::Model,
        media_type: "model/stl",
    },
];

pub fn view_only(path: &str) -> Option<&'static ViewOnly> {
    let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    VIEW_ONLY.iter().find(|v| v.extension == ext)
}

/// What a path's extension says it holds; text when ARK has no format for it.
pub fn kind_of(path: &str) -> Kind {
    view_only(path).map_or_else(
        || format_for(path).map_or(Kind::Text, |f| f.kind()),
        |v| v.kind,
    )
}

/// The artifact kind a stored copy of a file of this kind has.
pub fn artifact_kind(kind: Kind) -> ark_core::ArtifactKind {
    match kind {
        Kind::Text => ark_core::ArtifactKind::Text,
        Kind::Image => ark_core::ArtifactKind::Image,
        k => ark_core::ArtifactKind::named(k.as_str()),
    }
}

/// What the window shows when a file of these kinds is written, in one line for a tool's
/// description: "spreadsheets (xlsx, xlsm, csv, tsv), documents (docx), ...". From [`FORMATS`].
pub fn shown() -> String {
    let mut parts = Vec::new();
    for kind in Kind::ALL {
        let exts: Vec<&str> = FORMATS
            .iter()
            .filter(|f| f.kind() == kind)
            .flat_map(|f| f.extensions().iter().copied())
            .chain(
                VIEW_ONLY
                    .iter()
                    .filter(|v| v.kind == kind)
                    .map(|v| v.extension),
            )
            .collect();
        if !exts.is_empty() {
            parts.push(format!("{}s ({})", kind.noun(), exts.join(", ")));
        }
    }
    parts.push("other text files".into());
    parts.join(", ")
}

/// Classifies a stored artifact named by a path (a file a tool wrote) the way [`kind_of`]
/// classifies the file. Register it on the engine; a name ARK has no format for is left to the
/// next sniffer.
pub struct Kinds;

impl ark_core::Sniffer for Kinds {
    fn sniff(&self, name: &str, _bytes: &[u8]) -> Option<ark_core::ArtifactKind> {
        view_only(name)
            .map(|v| artifact_kind(v.kind))
            .or_else(|| format_for(name).map(|f| artifact_kind(f.kind())))
    }
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
    /// What the file is, from its format: a PDF is `Pdf` though it reads as a document.
    pub fn kind(&self) -> Kind {
        FORMATS
            .iter()
            .find(|f| f.name() == self.format)
            .map_or_else(|| self.content.kind(), |f| f.kind())
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
    let root = ark_os::dirs::canonicalize(root)?;
    let path = root.join(rel);
    // A link may point out of the folder: check where it really is. A new file's nearest
    // existing folder decides.
    let mut probe = path.clone();
    let mut rest = Vec::new();
    let real = loop {
        match ark_os::dirs::canonicalize(&probe) {
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
    // What does not exist yet will be made: a name every system accepts, so the project
    // folder opens anywhere (no `:`, `?` or Windows device names like `CON`).
    for name in &rest {
        if let Some(why) = ark_os::fs::name_problem(&name.to_string_lossy()) {
            return Err(Error::Path(format!("`{}`: {why}", name.to_string_lossy())));
        }
    }
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
    let root = ark_os::dirs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
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
    open_with_bytes(root, rel).map(|(file, _)| file)
}

/// Read a file of the project folder, with the bytes it was decoded from.
pub fn open_with_bytes(root: &Path, rel: &str) -> Result<(File, Vec<u8>), Error> {
    let path = inside(root, rel)?;
    let bytes = read_bytes(&path, rel)?;
    let (format, content) = decode(rel, &bytes)?;
    let file = File {
        path: relative(root, &path),
        format,
        version: version_of(&bytes),
        size: bytes.len() as u64,
        content,
    };
    Ok((file, bytes))
}

/// The editable source of a text-based format, even when the format is currently invalid.
pub fn source(root: &Path, rel: &str) -> Result<File, Error> {
    let view = view_only(rel).filter(|view| view.has_text_source());
    if !kind_of(rel).is_text() && view.is_none() {
        return Err(Error::Input("this file has no editable text source".into()));
    }
    let path = inside(root, rel)?;
    let bytes = if let Some(view) = view {
        use std::io::Read;
        let mut bytes = Vec::new();
        // Unix opens without following links; elsewhere `inside` already confined the path.
        #[cfg(unix)]
        let file = ark_os::fs::open_within(&root.canonicalize()?, &path)?;
        #[cfg(not(unix))]
        let file = std::fs::File::open(&path)?;
        file.take(view.max_bytes() + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > view.max_bytes() {
            return Err(Error::Format(format!(
                "Source exceeds the {} MiB view limit",
                view.max_bytes() / 1024 / 1024
            )));
        }
        bytes
    } else {
        read_bytes(&path, rel)?
    };
    let content = format::Text.read(&bytes)?;
    Ok(File {
        path: relative(root, &path),
        format: "text",
        version: version_of(&bytes),
        size: bytes.len() as u64,
        content,
    })
}

/// A file the window shows as itself (a PDF, an image): its bytes, its media type and version.
/// Other kinds are drawn from what they read as ([`File::page`]) and are refused here.
pub fn media(root: &Path, rel: &str) -> Result<(Vec<u8>, String, String), Error> {
    let path = inside(root, rel)?;
    let media_type = match kind_of(rel) {
        Kind::Pdf => "application/pdf".to_string(),
        Kind::Image => {
            let bytes = read_bytes(&path, rel)?;
            match format::Image.read(&bytes)? {
                Content::Media(m) => m.media_type,
                _ => unreachable!("an image reads as media"),
            }
        }
        k => {
            return Err(Error::Input(format!(
                "{rel} is a {}; it is shown from what it reads as",
                k.noun()
            )))
        }
    };
    let bytes = read_bytes(&path, rel)?;
    let version = version_of(&bytes);
    Ok((bytes, media_type, version))
}

/// The current version of a file, without reading it as its format.
pub fn version(root: &Path, rel: &str) -> Result<String, Error> {
    let path = inside(root, rel)?;
    Ok(version_of(&read_bytes(&path, rel)?))
}

/// Apply edits to a file and save it. With `base`, the edit is refused when the file is no
/// longer that version (someone else changed it since). Returns the file as saved.
pub fn edit(root: &Path, rel: &str, edits: &[Edit], base: Option<&str>) -> Result<File, Error> {
    if view_only(rel).is_some() {
        return Err(Error::Input(
            "Regenerate this model from its source to change it".into(),
        ));
    }
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
    if view_only(rel).is_some() {
        return Err(Error::Input(
            "Generate model files with a program, not text edits".into(),
        ));
    }
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
    let old = if path.is_file() {
        Some(std::fs::read(&path)?)
    } else {
        None
    };
    let text_file = format_for(rel).is_none_or(|f| f.kind().is_text());
    let bytes = match (content, &old) {
        // Rewriting a text file keeps how it is stored (CRLF lines, UTF-16).
        (Content::Text(t), Some(old)) if text_file => format::rewrite(old, t),
        _ => match format_for(rel) {
            Some(f) => {
                let bytes = f.create(content)?;
                match &old {
                    Some(old) if f.name() == "csv" => format::keep_charset(old, bytes),
                    _ => bytes,
                }
            }
            None => format::Text.create(content)?,
        },
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (format, content) = decode(rel, &bytes)?;
    save(&path, &bytes)?;
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
    if let Err(e) = ark_os::fs::replace(&tmp, path) {
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
    fn windows_file_in_use_errors_keep_the_original_diagnostic_and_source() {
        for code in [32, 33] {
            let original = std::io::Error::from_raw_os_error(code);
            let expected = original.to_string();
            let shown = io_error_message(&original, true);
            assert!(shown.starts_with(
                "file is in use by another application; close it there and try again: "
            ));
            assert!(shown.ends_with(&expected));

            let wrapped = Error::Io(original);
            assert!(std::error::Error::source(&wrapped).is_some());
        }
    }

    #[test]
    fn other_io_errors_keep_their_existing_display() {
        for (error, windows) in [
            (std::io::Error::from_raw_os_error(5), true),
            (std::io::Error::from_raw_os_error(32), false),
            (std::io::Error::other("ordinary failure"), true),
        ] {
            assert_eq!(io_error_message(&error, windows), error.to_string());
        }
    }

    #[test]
    fn maps_keep_editable_source_and_models_refuse_text_edits() {
        let dir = tempfile::tempdir().unwrap();
        let raw = "{\"type\":\"FeatureCollection\",\"features\":[]}";
        let first = write(
            dir.path(),
            "sites.geojson",
            &Content::Text(raw.into()),
            None,
        )
        .unwrap();
        assert_eq!(first.kind(), Kind::Map);
        assert!(write(
            dir.path(),
            "sites.geojson",
            &Content::Text("{broken".into()),
            Some(&first.version)
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sites.geojson")).unwrap(),
            raw
        );
        assert_eq!(
            source(dir.path(), "sites.geojson").unwrap().content,
            Content::Text(raw.into())
        );
        assert_eq!(kind_of("part.GLB"), Kind::Model);
        assert_eq!(kind_of("building.IFC"), Kind::Model);
        let ifc = view_only("building.ifc").unwrap();
        assert_eq!(ifc.max_bytes(), 2 * 1024 * 1024);
        assert!(ifc.preflight(b"ISO-10303-21;", 100).is_ok());
        assert!(ifc.preflight(b"not STEP", 8).is_err());
        assert!(write(
            dir.path(),
            "building.ifc",
            &Content::Text("bad".into()),
            None
        )
        .is_err());
        assert!(write(dir.path(), "part.stl", &Content::Text("bad".into()), None).is_err());
        std::fs::write(dir.path().join("bad.geojson"), "{broken").unwrap();
        assert!(open(dir.path(), "bad.geojson").is_err());
        assert_eq!(
            source(dir.path(), "bad.geojson").unwrap().content,
            Content::Text("{broken".into())
        );
        assert!(view_only("part.glb").unwrap().preflight(b"bad", 3).is_err());
        std::fs::write(dir.path().join("building.ifc"), "invalid IFC source").unwrap();
        assert_eq!(
            source(dir.path(), "building.ifc").unwrap().content,
            Content::Text("invalid IFC source".into())
        );
        assert!(edit(dir.path(), "building.ifc", &[], None).is_err());
        std::fs::File::create(dir.path().join("building.ifc"))
            .unwrap()
            .set_len(2 * 1024 * 1024 + 1)
            .unwrap();
        assert!(source(dir.path(), "building.ifc").is_err());
    }

    /// An agent that writes the whole sheet over an Excel CSV keeps its Windows-1252 bytes.
    #[test]
    fn rewriting_a_windows_1252_csv_as_a_sheet_keeps_its_encoding() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.csv"), b"name,city\nJos\xE9,C\xF3rdoba\n").unwrap();
        let book = open(dir.path(), "a.csv").unwrap().content;
        write(dir.path(), "a.csv", &book, None).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("a.csv")).unwrap(),
            b"name,city\nJos\xE9,C\xF3rdoba\n"
        );
    }

    /// The page's editor saves the whole text: a CRLF file stays CRLF.
    #[test]
    fn rewriting_a_crlf_file_keeps_crlf() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "one\r\ntwo\r\n").unwrap();
        write(
            dir.path(),
            "notes.txt",
            &Content::Text("uno\ndos\n".into()),
            None,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("notes.txt")).unwrap(),
            b"uno\r\ndos\r\n"
        );
    }

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
        for bad in [
            "notes/a:b.md",
            "CON",
            "nul.txt",
            "notes/new?/x.md",
            "draft.",
        ] {
            assert!(inside(root, bad).is_err(), "a new file named {bad}");
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
            ..Workbook::default()
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
    fn a_files_kind_is_decided_once_for_paths_and_stored_copies() {
        use ark_core::{ArtifactKind, Sniffer};
        assert_eq!(kind_of("out/chart.PNG"), Kind::Image);
        assert_eq!(kind_of("report.pdf"), Kind::Pdf);
        assert_eq!(kind_of("notes.md"), Kind::Markdown);
        assert_eq!(kind_of("run.py"), Kind::Text);
        assert_eq!(Kinds.sniff("out/chart.png", b""), Some(ArtifactKind::Image));
        assert_eq!(
            Kinds.sniff("t.csv", b""),
            Some(ArtifactKind::named("sheet"))
        );
        assert_eq!(Kinds.sniff("r.pdf", b""), Some(ArtifactKind::named("pdf")));
        assert_eq!(
            Kinds.sniff("run.py", b"print(1)"),
            None,
            "left to the next sniffer"
        );
        assert_eq!(
            shown(),
            "Markdown documents (md, markdown), spreadsheets (xlsx, xlsm, csv, tsv), documents (docx), PDFs (pdf), images (png, jpg, jpeg, gif, webp, svg), maps (geojson), 3D models (ifc, glb, stl), other text files"
        );
    }

    #[test]
    fn unknown_binary_files_are_not_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), [0u8, 1, 2]).unwrap();
        assert!(open(dir.path(), "a.bin").is_err());
        std::fs::write(dir.path().join("b.md"), "# hi").unwrap();
        assert_eq!(open(dir.path(), "b.md").unwrap().kind(), Kind::Markdown);
        assert_eq!(
            open(dir.path(), "b.md").unwrap().content,
            Content::Text("# hi".into())
        );
        assert_eq!(kind_of("x.XLSX"), Kind::Sheet);
    }
}
