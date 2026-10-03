//! The window's file browser: list, read and save files in the project's folder. A path that
//! leaves the folder (`..`, an absolute path, a link pointing out) is refused. Saving is the
//! person's own action, like typing in the terminal; the agent's writes still go through its
//! tools and the approval gate.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

/// Entries listed per folder; the rest are counted.
const MAX_ENTRIES: usize = 500;
/// Largest file shown as text.
const MAX_READ: u64 = 2 * 1024 * 1024;

#[derive(Serialize)]
pub(crate) struct Entry {
    pub name: String,
    /// Relative to the project folder, with `/`.
    pub path: String,
    pub dir: bool,
    pub size: u64,
}

/// `rel` inside `root`, or an error. `rel` may be empty (the root itself).
fn inside(root: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    let rel = Path::new(rel.trim().trim_start_matches("./"));
    if rel
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        anyhow::bail!("`{}` is outside the project folder", rel.display());
    }
    let root = root.canonicalize()?;
    let path = root.join(rel);
    // A link may point out of the folder: check where it really is when it exists.
    let real = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => path
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .map(|p| p.join(path.file_name().unwrap_or_default()))
            .ok_or_else(|| anyhow::anyhow!("no folder for `{}`", rel.display()))?,
    };
    if !real.starts_with(&root) {
        anyhow::bail!("`{}` is outside the project folder", rel.display());
    }
    Ok(real)
}

fn relative(root: &Path, p: &Path) -> String {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    p.strip_prefix(&root)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// One folder's entries, folders first, hidden ones left out. Returns the entries and how
/// many more there were.
pub(crate) fn list(root: &Path, rel: &str) -> anyhow::Result<(Vec<Entry>, usize)> {
    let dir = inside(root, rel)?;
    let mut v: Vec<Entry> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some(Entry {
                name: e.file_name().to_string_lossy().into_owned(),
                path: relative(root, &e.path()),
                dir: meta.is_dir(),
                size: meta.len(),
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

/// A file's text. A binary or very large file comes back without text.
pub(crate) fn read(root: &Path, rel: &str) -> anyhow::Result<serde_json::Value> {
    let path = inside(root, rel)?;
    let meta = std::fs::metadata(&path)?;
    if meta.is_dir() {
        anyhow::bail!("`{rel}` is a folder");
    }
    let path_s = relative(root, &path);
    if meta.len() > MAX_READ {
        return Ok(
            serde_json::json!({ "path": path_s, "size": meta.len(), "text": null, "why": "too large to show" }),
        );
    }
    let bytes = std::fs::read(&path)?;
    if bytes.contains(&0) {
        return Ok(
            serde_json::json!({ "path": path_s, "size": meta.len(), "text": null, "why": "not text" }),
        );
    }
    Ok(
        serde_json::json!({ "path": path_s, "size": meta.len(), "text": String::from_utf8_lossy(&bytes) }),
    )
}

/// Save `text` to a file in the folder (its parent must exist).
pub(crate) fn write(root: &Path, rel: &str, text: &str) -> anyhow::Result<String> {
    let path = inside(root, rel)?;
    if path.is_dir() {
        anyhow::bail!("`{rel}` is a folder");
    }
    std::fs::write(&path, text)?;
    Ok(relative(root, &path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/a.md"), "hello").unwrap();
        std::fs::write(root.join(".hidden"), "x").unwrap();

        let (top, more) = list(root, "").unwrap();
        assert_eq!(more, 0);
        assert_eq!(top.len(), 1, "hidden files are left out");
        assert!(top[0].dir && top[0].path == "notes");
        assert_eq!(read(root, "notes/a.md").unwrap()["text"], "hello");
        assert_eq!(write(root, "notes/b.md", "new").unwrap(), "notes/b.md");
        assert_eq!(
            std::fs::read_to_string(root.join("notes/b.md")).unwrap(),
            "new"
        );

        for bad in ["../x", "/etc/passwd", "notes/../../x"] {
            assert!(read(root, bad).is_err(), "{bad}");
            assert!(write(root, bad, "x").is_err(), "{bad}");
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
}
