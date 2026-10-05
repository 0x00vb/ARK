//! A folder sent to this runtime from another computer becomes a project here (ADR 0030).
//!
//! The client starts an upload, sends the files in parts (`upload_parts`, several small files or
//! a part of a large one per call) into a folder of its own in the store
//! (`<store>/uploads/<id>`), then asks for the project (`upload_project`): the folder moves in
//! one step to where a new project keeps its files, and the project gets the name asked for (made
//! unique). Until then nothing shows: a copy that stops halfway leaves no project, and
//! `upload_cancel` (or a start of the runtime a day later) removes what it left.
//!
//! Files dropped on a chat take the same way into the open project instead (`upload_add`, ADR
//! 0035): each keeps its name, or gets a number when the name is taken.
//!
//! Every call can be sent again after a lost answer: a part already written is not written
//! twice, and asking twice for the project answers the same project.
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

/// The most one call carries, decoded. Senders use less (`remote::send_folder`).
pub(crate) const MAX_CALL: usize = 8 * 1024 * 1024;
/// An upload nobody finished or cancelled is removed after this long.
const STALE: Duration = Duration::from_secs(24 * 60 * 60);

/// Projects are made one at a time, so two uploads never get the same name.
static NAMING: Mutex<()> = Mutex::new(());

/// The folder of upload `id`. Ids are the client's: letters, digits and `-`, 8 to 64.
fn folder(store: &Path, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "an upload id is 8 to 64 letters, digits or `-`"
    );
    Ok(store.join("uploads").join(id))
}

/// Where a finished upload keeps the project it became, so asking again answers it.
fn done_file(dir: &Path) -> PathBuf {
    dir.with_extension("done")
}

/// Write parts (`[{path, data, offset}]`) of upload `id`. No parts starts the upload. At
/// `offset` 0 a file starts again; otherwise the part goes at its end, which must be `offset`
/// (or already `offset` plus the part: a part sent again). Answers `{written}`, the bytes taken.
pub(crate) fn put(store: &Path, id: &str, parts: &Value) -> anyhow::Result<Value> {
    let dir = folder(store, id)?;
    anyhow::ensure!(
        !done_file(&dir).exists(),
        "upload `{id}` is a project already"
    );
    let parts = match parts {
        Value::Null => &[][..],
        Value::Array(a) => a.as_slice(),
        _ => anyhow::bail!("`parts` is a list"),
    };
    let mut decoded = Vec::with_capacity(parts.len());
    let mut total = 0;
    for p in parts {
        let data = p["data"].as_str().unwrap_or("");
        let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
        total += bytes.len();
        anyhow::ensure!(total <= MAX_CALL, "a call carries at most {MAX_CALL} bytes");
        let rel = p["path"].as_str().unwrap_or("").trim().to_string();
        anyhow::ensure!(!rel.is_empty(), "a file needs a path");
        decoded.push((rel, p["offset"].as_u64().unwrap_or(0), bytes));
    }
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
    }
    for (rel, offset, bytes) in &decoded {
        write_part(&dir, rel, *offset, bytes)?;
    }
    Ok(json!({ "written": total }))
}

fn write_part(dir: &Path, rel: &str, offset: u64, bytes: &[u8]) -> anyhow::Result<()> {
    let path = ark_files::inside(dir, rel)?;
    anyhow::ensure!(!path.is_dir(), "`{rel}` is a folder");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if offset == 0 {
        std::fs::File::create(&path)?.write_all(bytes)?;
        return Ok(());
    }
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if size == offset + bytes.len() as u64 {
        // Written before; the answer was lost.
        return Ok(());
    }
    anyhow::ensure!(
        size == offset,
        "`{rel}` has {size} bytes here, not {offset}; send it again from the start"
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(bytes)?;
    Ok(())
}

/// The upload's folder becomes a new project's, named `name` (or `name 2`, … when taken).
/// Answers `{id, name}`, the same when asked again.
pub(crate) fn finish(engine: &Engine, id: &str, name: &str) -> anyhow::Result<Value> {
    let store = engine.store.root();
    let dir = folder(store, id)?;
    let done = done_file(&dir);
    let _one = NAMING.lock().unwrap_or_else(|e| e.into_inner());
    if let Ok(v) = std::fs::read_to_string(&done) {
        let answer: Value = serde_json::from_str(&v)?;
        anyhow::ensure!(
            answer.get("id").is_some(),
            "upload `{id}` went into a project already"
        );
        return Ok(answer);
    }
    anyhow::ensure!(dir.is_dir(), "there is no upload `{id}`; start it again");
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "a project needs a name");
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
    let project = Project::new(&name, "");
    // Where a project without a chosen folder keeps its files (`Engine::workspace_dir`), in
    // the same store, so the move is one rename. The project is stored only once its folder is
    // there: a failed move leaves no project.
    let target = store.join("workspaces").join(project.id.as_str());
    std::fs::create_dir_all(store.join("workspaces"))?;
    std::fs::rename(&dir, &target)?;
    if let Err(e) = engine.store.put_project(&project) {
        let _ = std::fs::rename(&target, &dir);
        return Err(ark_runtime::AgentError::from(e).into());
    }
    let answer = json!({ "id": project.id.as_str(), "name": project.name });
    std::fs::write(&done, answer.to_string())?;
    Ok(answer)
}

/// The upload's files go into the project folder `dir`. Each file or folder at the top of the
/// upload keeps its name, or gets a number when the name is taken there (`sales 2.xlsx`); nothing
/// is overwritten. Answers `{added: [paths]}`, the same when asked again.
pub(crate) fn add(store: &Path, id: &str, dir: &Path) -> anyhow::Result<Value> {
    let up = folder(store, id)?;
    let done = done_file(&up);
    let _one = NAMING.lock().unwrap_or_else(|e| e.into_inner());
    if let Ok(v) = std::fs::read_to_string(&done) {
        let answer: Value = serde_json::from_str(&v)?;
        anyhow::ensure!(
            answer.get("added").is_some(),
            "upload `{id}` is a project already"
        );
        return Ok(answer);
    }
    anyhow::ensure!(up.is_dir(), "there is no upload `{id}`; start it again");
    std::fs::create_dir_all(dir)?;
    let mut names: Vec<String> = std::fs::read_dir(&up)?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let mut added = Vec::new();
    for name in names {
        let free = free_name(dir, &name);
        let target = ark_files::inside(dir, &free)?;
        move_entry(&up.join(&name), &target)?;
        added.push(free);
    }
    let answer = json!({ "added": added });
    std::fs::write(&done, answer.to_string())?;
    let _ = std::fs::remove_dir_all(&up);
    Ok(answer)
}

/// `name`, or `name 2`, `name 3`, … before the extension (`sales 2.xlsx`), the first not taken
/// in `dir`.
fn free_name(dir: &Path, name: &str) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    std::iter::once(name.to_string())
        .chain((2..).map(|i| format!("{stem} {i}{ext}")))
        .find(|n| !dir.join(n).exists())
        .unwrap_or_default()
}

/// Move a file or folder; across disks, copy it and remove the first.
fn move_entry(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_all(from, to)?;
    if from.is_dir() {
        std::fs::remove_dir_all(from)
    } else {
        std::fs::remove_file(from)
    }
}

fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    if !from.is_dir() {
        return std::fs::copy(from, to).map(|_| ());
    }
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        copy_all(&e.path(), &to.join(e.file_name()))?;
    }
    Ok(())
}

/// Remove an upload and what it wrote. Nothing to remove is fine.
pub(crate) fn cancel(store: &Path, id: &str) -> anyhow::Result<()> {
    let dir = folder(store, id)?;
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Remove uploads, and answers kept for finished ones, older than a day (a client that stopped
/// and never came back). When the runtime starts.
pub(crate) fn sweep(store: &Path) {
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
            let p = e.path();
            let _ = if p.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(path: &str, data: &[u8], offset: u64) -> Value {
        json!({ "path": path, "offset": offset,
                "data": base64::engine::general_purpose::STANDARD.encode(data) })
    }

    #[test]
    fn parts_add_up_once_and_paths_stay_inside() {
        let store = tempfile::tempdir().unwrap();
        let s = store.path();
        let id = "abcdef12";
        put(s, id, &Value::Null).unwrap();
        assert!(
            s.join("uploads").join(id).is_dir(),
            "no parts starts the upload"
        );
        put(
            s,
            id,
            &json!([part("a/b.txt", b"hello ", 0), part("c.txt", b"", 0)]),
        )
        .unwrap();
        let world = json!([part("a/b.txt", b"world", 6)]);
        put(s, id, &world).unwrap();
        // The same part again (its answer was lost) is not written twice.
        put(s, id, &world).unwrap();
        let file = s.join("uploads").join(id).join("a/b.txt");
        assert_eq!(std::fs::read(&file).unwrap(), b"hello world");
        assert_eq!(
            std::fs::read(s.join("uploads").join(id).join("c.txt")).unwrap(),
            b""
        );
        // A part at the wrong place is refused, and the file is left as it was.
        assert!(put(s, id, &json!([part("a/b.txt", b"!", 3)])).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"hello world");
        // Out of the folder, or a bad id.
        assert!(put(s, id, &json!([part("../x", b"x", 0)])).is_err());
        assert!(put(s, id, &json!([part("/etc/x", b"x", 0)])).is_err());
        assert!(put(s, "../../x", &json!([part("x", b"x", 0)])).is_err());
        assert!(put(s, "short", &Value::Null).is_err());
        cancel(s, id).unwrap();
        assert!(!s.join("uploads").join(id).exists());
        cancel(s, id).unwrap();
    }

    /// ADR 0035: files dropped on a chat land in the project folder under their names, or with
    /// a number when a name is taken; asking again answers the same.
    #[test]
    fn files_added_to_a_project_keep_their_names_or_get_a_number() {
        let store = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (s, dir) = (store.path(), project.path());
        std::fs::write(dir.join("sales.xlsx"), b"theirs").unwrap();
        std::fs::create_dir(dir.join("exports")).unwrap();
        let id = "drop-0001";
        put(
            s,
            id,
            &json!([
                part("sales.xlsx", b"mine", 0),
                part("README", b"r", 0),
                part("exports/2026/orders.csv", b"a,b", 0),
            ]),
        )
        .unwrap();
        let answer = add(s, id, dir).unwrap();
        assert_eq!(
            answer,
            json!({ "added": ["README", "exports 2", "sales 2.xlsx"] })
        );
        assert_eq!(std::fs::read(dir.join("sales.xlsx")).unwrap(), b"theirs");
        assert_eq!(std::fs::read(dir.join("sales 2.xlsx")).unwrap(), b"mine");
        assert_eq!(
            std::fs::read(dir.join("exports 2/2026/orders.csv")).unwrap(),
            b"a,b"
        );
        assert!(!s.join("uploads").join(id).exists());
        // The answer was lost: asking again moves nothing more.
        assert_eq!(add(s, id, dir).unwrap(), answer);
        assert!(!dir.join("sales 3.xlsx").exists());
        // An upload made into a project is not added to another, and nothing is not an upload.
        put(s, "drop-0002", &json!([part("x.txt", b"x", 0)])).unwrap();
        std::fs::write(
            done_file(&folder(s, "drop-0002").unwrap()),
            json!({ "id": "p", "name": "x" }).to_string(),
        )
        .unwrap();
        assert!(add(s, "drop-0002", dir).is_err());
        // And an upload added to a project is not made a project.
        let engine = ark_sdk::prelude::Engine::open(s).unwrap();
        assert!(finish(&engine, id, "x").is_err());
        assert!(add(s, "drop-0003", dir).is_err());
        assert_eq!(free_name(dir, ".env"), ".env");
        assert_eq!(free_name(dir, "README"), "README 2");
    }

    #[test]
    fn a_bad_part_writes_nothing_of_its_call() {
        let store = tempfile::tempdir().unwrap();
        let s = store.path();
        let id = "abcdef34";
        let r = put(s, id, &json!([part("ok.txt", b"x", 0), part("", b"y", 0)]));
        assert!(r.is_err());
        assert!(!s.join("uploads").join(id).join("ok.txt").exists());
    }
}
