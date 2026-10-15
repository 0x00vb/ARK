//! The window's files: list the project's folder, open a file (text, a spreadsheet or a
//! document, as `ark_files` reads it), and save the person's changes to the same file the agent
//! works on. A path that leaves the folder is refused. Saving is the person's own action, like
//! typing in the terminal; the agent's writes still go through its tools and the approval gate.

use std::path::Path;

use ark_sdk::prelude::ark_files::{self, Content, Edit};
use serde::Serialize;
use serde_json::{json, Value};

/// Entries listed per folder; the rest are counted.
const MAX_ENTRIES: usize = 500;

#[derive(Serialize)]
pub(crate) struct Entry {
    pub name: String,
    /// Relative to the project folder, with `/`.
    pub path: String,
    pub dir: bool,
    pub size: u64,
    /// The file's kind (`text`, `sheet`, `pdf`, ...; `ark_files::Kind`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
}

/// One folder's entries, folders first, hidden ones left out. Returns the entries and how
/// many more there were.
pub(crate) fn list(root: &Path, rel: &str) -> anyhow::Result<(Vec<Entry>, usize)> {
    let dir = ark_files::inside(root, rel)?;
    let mut v: Vec<Entry> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let path = ark_files::relative(root, &e.path());
            let kind = (!meta.is_dir()).then(|| ark_files::kind_of(&path).as_str());
            Some(Entry {
                name: e.file_name().to_string_lossy().into_owned(),
                path,
                dir: meta.is_dir(),
                size: meta.len(),
                kind,
            })
        })
        .collect();
    v.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let more = v.len().saturating_sub(MAX_ENTRIES);
    v.truncate(MAX_ENTRIES);
    Ok((v, more))
}

/// A file as the window draws it: `kind`, `version`, and its `text`, `sheets` or `blocks`. A
/// file that cannot be shown comes back without them, with `why`.
pub(crate) fn read(root: &Path, rel: &str) -> anyhow::Result<Value> {
    let path = ark_files::inside(root, rel)?;
    let meta = std::fs::metadata(&path)?;
    if meta.is_dir() {
        anyhow::bail!("`{rel}` is a folder");
    }
    let shown = ark_files::relative(root, &path);
    if let Some(view) = ark_files::view_only(rel) {
        return Ok(
            json!({ "path": shown, "kind": view.kind.as_str(), "format": view.extension,
            "media_type": view.media_type, "size": meta.len(), "view_only": true, "snapshot_supported": cfg!(unix) }),
        );
    }
    match ark_files::open(root, rel) {
        Ok(file) => {
            let mut page = file.page();
            if file.kind() == ark_files::Kind::Map {
                page["snapshot_supported"] = json!(cfg!(unix));
            }
            Ok(page)
        }
        Err(ark_files::Error::Format(why)) => {
            let why = if meta.len() > ark_files::MAX_BYTES {
                "too large to show".to_string()
            } else if ark_files::format_for(rel).is_some() {
                why
            } else {
                "not text".to_string()
            };
            Ok(
                json!({ "path": shown, "size": meta.len(), "text": null, "why": why,
                "kind": (ark_files::kind_of(rel) == ark_files::Kind::Map).then_some("map"), "snapshot_supported": cfg!(unix) }),
            )
        }
        Err(e) => Err(e.into()),
    }
}

/// Save `text` to a text file (its parent must exist). With `base`, refused when the file
/// changed since that version.
pub(crate) fn write(
    root: &Path,
    rel: &str,
    text: &str,
    base: Option<&str>,
) -> anyhow::Result<Value> {
    let path = ark_files::inside(root, rel)?;
    if path.is_dir() {
        anyhow::bail!("`{rel}` is a folder");
    }
    if !ark_files::kind_of(rel).is_text() {
        anyhow::bail!("`{rel}` is not a text file");
    }
    if !path.parent().is_some_and(Path::is_dir) {
        anyhow::bail!("there is no folder for `{rel}`");
    }
    let file = ark_files::write(root, rel, &Content::Text(text.to_string()), base)?;
    Ok(json!({ "path": file.path, "version": file.version }))
}

/// Apply the person's edits (the same ones the agent sends) and answer the file as saved.
pub(crate) fn edit(
    root: &Path,
    rel: &str,
    edits: &Value,
    base: Option<&str>,
) -> anyhow::Result<Value> {
    let edits = Edit::parse_all(edits)?;
    Ok(ark_files::edit(root, rel, &edits, base)?.page())
}

/// A PDF or an image as the window draws it: the file's own bytes (base64), its media type and
/// version. Through `call` like everything else, so the desktop and a browser get it the same way.
pub(crate) fn media(root: &Path, rel: &str) -> anyhow::Result<Value> {
    use base64::Engine as _;
    let (bytes, media_type, version) = ark_files::media(root, rel)?;
    Ok(json!({
        "media_type": media_type,
        "version": version,
        "size": bytes.len(),
        "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
    }))
}

/// Open a spreadsheet or document in the app the computer opens it with. Only those kinds: a
/// click never runs a script or a program from the folder.
pub(crate) fn open_in_app(root: &Path, rel: &str) -> anyhow::Result<()> {
    let path = ark_files::inside(root, rel)?;
    if ark_files::kind_of(rel) == ark_files::Kind::Text
        || (ark_files::format_for(rel).is_none() && ark_files::view_only(rel).is_none())
    {
        anyhow::bail!("only files ARK has a format for open in their app");
    }
    if !path.is_file() {
        anyhow::bail!("there is no file {rel}");
    }
    ark_os::open::open(&path)?;
    Ok(())
}

pub(crate) fn version(root: &Path, rel: &str) -> anyhow::Result<Value> {
    Ok(json!({ "version": ark_files::version(root, rel)? }))
}

/// The extension's selection is model input, not an unbounded document transport.
pub(crate) fn validate_focus(focus: &ark_sdk::prelude::ark_core::Focus) -> anyhow::Result<()> {
    if !matches!(
        focus.surface.as_ref().map(|s| s.kind_tag.as_str()),
        Some("map" | "model")
    ) {
        return Ok(());
    }
    let Some(selection) = &focus.selection else {
        return Ok(());
    };
    anyhow::ensure!(
        serde_json::to_vec(selection)?.len() <= 2048,
        "file selection exceeds 2 KiB"
    );
    for (key, value) in &selection.marks {
        match key.as_str() {
            "id" | "label" | "geometry_type" | "units" | "measured_from" => {
                anyhow::ensure!(
                    value.as_str().is_some_and(|v| v.chars().count() <= 120),
                    "invalid file selection label"
                );
            }
            "properties" => {
                let properties = value
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("invalid selection properties"))?;
                anyhow::ensure!(
                    properties.len() <= 16,
                    "file selection exceeds 16 properties"
                );
                for (k, v) in properties {
                    anyhow::ensure!(
                        k.chars().count() <= 64
                            && !v.is_object()
                            && !v.is_array()
                            && v.as_str().is_none_or(|s| s.chars().count() <= 256),
                        "invalid selection property"
                    );
                }
            }
            "bounds" | "points" => {
                let values = value
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("invalid selection coordinates"))?;
                anyhow::ensure!(
                    values.len() <= 6
                        && values
                            .iter()
                            .all(|v| v.as_f64().is_some_and(f64::is_finite)),
                    "invalid selection coordinates"
                );
            }
            "count" | "distance" | "triangle" => anyhow::ensure!(
                value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0),
                "invalid selection number"
            ),
            _ => anyhow::bail!("unsupported file selection field"),
        }
    }
    anyhow::ensure!(
        selection.text.is_none() && selection.line.is_none(),
        "file selection must use bounded fields"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_focus_refuses_property_blobs_and_long_values() {
        let focus = |selection: Value| {
            serde_json::from_value(json!({
                "surface": { "kind_tag": "map", "path": "sites.geojson" }, "selection": selection
            }))
            .unwrap()
        };
        assert!(validate_focus(&focus(
            json!({ "id": "one", "properties": { "name": "Site" }, "bounds": [1,2,3,4] })
        ))
        .is_ok());
        for selection in [
            json!({ "properties": { "nested": { "x": 1 } } }),
            json!({ "label": "x".repeat(121) }),
            json!({ "geometry": [1,2,3] }),
            json!({ "properties": { "name": "x".repeat(3000) } }),
        ] {
            assert!(validate_focus(&focus(selection)).is_err());
        }
    }

    #[test]
    fn paths_stay_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/a.md"), "hello").unwrap();
        std::fs::write(root.join(".hidden"), "x").unwrap();
        std::fs::write(root.join("bin"), [0u8, 1]).unwrap();

        let (top, more) = list(root, "").unwrap();
        assert_eq!(more, 0);
        assert_eq!(top.len(), 2, "hidden files are left out");
        assert!(top[0].dir && top[0].path == "notes");
        assert_eq!(read(root, "notes/a.md").unwrap()["text"], "hello");
        assert_eq!(read(root, "bin").unwrap()["why"], "not text");
        assert_eq!(
            write(root, "notes/b.md", "new", None).unwrap()["path"],
            "notes/b.md"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("notes/b.md")).unwrap(),
            "new"
        );
        assert!(write(root, "nowhere/b.md", "new", None).is_err());

        for bad in ["../x", "/etc/passwd", "notes/../../x"] {
            assert!(read(root, bad).is_err(), "{bad}");
            assert!(write(root, bad, "x", None).is_err(), "{bad}");
        }
        #[cfg(unix)]
        {
            let out = tempfile::tempdir().unwrap();
            std::fs::write(out.path().join("secret"), "s").unwrap();
            std::os::unix::fs::symlink(out.path(), root.join("link")).unwrap();
            assert!(
                read(root, "link/secret").is_err(),
                "a link out of the folder"
            );
        }
    }

    #[test]
    fn the_person_edits_a_spreadsheet_like_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("t.csv"), "a,b\n1,2\n").unwrap();
        let v = read(root, "t.csv").unwrap();
        assert_eq!(v["kind"], "sheet");
        let base = v["version"].as_str().unwrap().to_string();
        let after = edit(
            root,
            "t.csv",
            &json!([{"cell": "C2", "value": "=A2+B2"}]),
            Some(&base),
        )
        .unwrap();
        assert_eq!(after["sheets"][0]["rows"][1][2]["t"], "3");
        assert!(
            edit(
                root,
                "t.csv",
                &json!([{"cell": "A1", "value": 1}]),
                Some(&base)
            )
            .is_err(),
            "a stale version"
        );
        assert_eq!(version(root, "t.csv").unwrap()["version"], after["version"]);
        assert_eq!(list(root, "").unwrap().0[0].kind, Some("sheet"));
    }
}
