//! A folder sent to this runtime from another computer becomes a project here (ADR 0030).
//!
//! The client sends each file in parts (`upload_file`) into a folder of its own in the store
//! (`<store>/uploads/<id>`), then asks for the project (`upload_project`): the folder moves in
//! one step to where a new project keeps its files, and the project gets the name asked for (made
//! unique). Until then nothing shows: a copy that stops halfway leaves no project, and
//! `upload_cancel` (or the next upload a day later) removes what it left.
//!
//! Paths stay inside the upload's folder (`ark_files::inside`). Links are not sent
//! (`remote::send_folder` skips them), and none can be made here: only files are written.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use ark_sdk::prelude::ark_files;
use ark_sdk::prelude::ark_runtime;
use ark_sdk::prelude::*;
use base64::Engine as _;
use serde_json::{json, Value};

/// The largest part one call carries, decoded. Senders use less (`remote::send_folder`).
pub(crate) const MAX_PART: usize = 8 * 1024 * 1024;
/// An upload nobody finished or cancelled is removed after this long.
const STALE: Duration = Duration::from_secs(24 * 60 * 60);

/// Projects are named one at a time, so two uploads never get the same name.
static NAMING: Mutex<()> = Mutex::new(());

/// The folder of upload `id`. Ids are the client's: letters, digits and `-`, 8 to 64.
fn folder(store: &Path, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "an upload id is 8 to 64 letters, digits or `-`"
    );
    Ok(store.join("uploads").join(id))
}

/// Write one part of a file: at `offset` 0 the file starts again; otherwise the part goes at
/// the end, which must be `offset`. Answers the file's size so far.
pub(crate) fn put(
    store: &Path,
    id: &str,
    rel: &str,
    data: &str,
    offset: u64,
) -> anyhow::Result<Value> {
    let dir = folder(store, id)?;
    if offset == 0 && !dir.exists() {
        sweep(store);
        std::fs::create_dir_all(&dir)?;
    }
    anyhow::ensure!(dir.is_dir(), "there is no upload `{id}`; start it again");
    let rel = rel.trim();
    anyhow::ensure!(!rel.is_empty(), "a file needs a path");
    let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
    anyhow::ensure!(
        bytes.len() <= MAX_PART,
        "a part is at most {MAX_PART} bytes"
    );
    let path = ark_files::inside(&dir, rel)?;
    anyhow::ensure!(!path.is_dir(), "`{rel}` is a folder");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = if offset == 0 {
        std::fs::File::create(&path)?
    } else {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        anyhow::ensure!(
            size == offset,
            "`{rel}` has {size} bytes here, not {offset}; send it again from the start"
        );
        std::fs::OpenOptions::new().append(true).open(&path)?
    };
    file.write_all(&bytes)?;
    Ok(json!({ "size": offset + bytes.len() as u64 }))
}

/// The upload's folder becomes a new project's, named `name` (or `name 2`, … when taken).
pub(crate) fn finish(engine: &Engine, id: &str, name: &str) -> anyhow::Result<Project> {
    let store = engine.store.root();
    let dir = folder(store, id)?;
    anyhow::ensure!(dir.is_dir(), "there is no upload `{id}`; start it again");
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "a project needs a name");
    let _one = NAMING.lock().unwrap_or_else(|e| e.into_inner());
    let taken: Vec<String> = engine
        .store
        .list_projects()
        .map_err(ark_runtime::AgentError::from)?
        .into_iter()
        .filter(|p| p.parent.is_none())
        .map(|p| p.name)
        .collect();
    let free = |n: &str| !taken.iter().any(|t| t == n);
    let name = std::iter::once(name.to_string())
        .chain((2..).map(|i| format!("{name} {i}")))
        .find(|n| free(n))
        .unwrap_or_default();
    let project = engine.create_project(&name, "")?;
    // Where a project without a chosen folder keeps its files (`Engine::workspace_dir`): in
    // the same store, so the move is one rename.
    let target = store.join("workspaces").join(project.id.as_str());
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if target.exists() {
        std::fs::remove_dir(&target)?;
    }
    std::fs::rename(&dir, &target)?;
    Ok(project)
}

/// Remove an upload and what it wrote. Nothing to remove is fine.
pub(crate) fn cancel(store: &Path, id: &str) -> anyhow::Result<()> {
    let dir = folder(store, id)?;
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Remove uploads left for more than a day (a client that stopped and never came back).
fn sweep(store: &Path) {
    let Ok(entries) = std::fs::read_dir(store.join("uploads")) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > STALE);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn parts_add_up_and_paths_stay_inside() {
        let store = tempfile::tempdir().unwrap();
        let s = store.path();
        let id = "abcdef12";
        put(s, id, "a/b.txt", &b64(b"hello "), 0).unwrap();
        let v = put(s, id, "a/b.txt", &b64(b"world"), 6).unwrap();
        assert_eq!(v["size"], 11);
        let file = s.join("uploads").join(id).join("a/b.txt");
        assert_eq!(std::fs::read(&file).unwrap(), b"hello world");
        // A part at the wrong place is refused, and the file is left as it was.
        assert!(put(s, id, "a/b.txt", &b64(b"!"), 3).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"hello world");
        // Out of the folder, or a bad id.
        assert!(put(s, id, "../x", &b64(b"x"), 0).is_err());
        assert!(put(s, id, "/etc/x", &b64(b"x"), 0).is_err());
        assert!(put(s, "../../x", "x", &b64(b"x"), 0).is_err());
        assert!(put(s, "short", "x", &b64(b"x"), 0).is_err());
        // A later part of an upload that is not there.
        assert!(put(s, "abcdef99", "x", &b64(b"x"), 4).is_err());
        cancel(s, id).unwrap();
        assert!(!s.join("uploads").join(id).exists());
        cancel(s, id).unwrap();
    }
}
