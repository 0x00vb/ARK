//! What a piece of work changed in a project folder, found from the folder itself (ADR 0034,
//! ADR 0043 › A worker's results). Before the work: a listing of the files and a copy of each
//! (bounded clones where the disk can: APFS, btrfs, XFS). After: each file that is new
//! or changed is written by the work; each file changed or removed keeps its copy as the
//! version before.
//!
//! Run code takes it around one program; work an agent does elsewhere takes it around a whole
//! task, so the listing can be saved and read back after a restart.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ark_core::ArtifactKind;
use ark_tools::ProducedArtifact;
use serde::{Deserialize, Serialize};

/// Entries looked at before a listing stops (a project past it is only partly listed).
const MAX_WALK: usize = 20_000;
/// Folders never looked into: package caches and build output, rebuilt from elsewhere.
pub const SKIPPED: &[&str] = &["node_modules", "target", "__pycache__", "venv", ".venv"];
/// The largest file copied, and the most copied in one listing.
const BEFORE_FILE: u64 = 256 * 1024 * 1024;
const BEFORE_TOTAL: u64 = 512 * 1024 * 1024;
/// Versions before kept from one comparison; past it, changes are listed as not kept.
pub const MAX_VERSIONS: usize = 50;

/// Capture a regular project file with a byte limit enforced on its opened handle.
pub fn read_capture(root: &Path, relative: &str, max: u64) -> std::io::Result<Vec<u8>> {
    ark_os::fs::open_capture(root, &root.join(relative))
        .and_then(|file| ark_os::fs::read_bounded(file, max))
}

/// A file as a listing saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub len: u64,
    /// Modification time, in nanoseconds since the Unix epoch.
    pub modified_ns: u128,
}

impl Seen {
    fn modified(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_nanos(self.modified_ns.min(u64::MAX as u128) as u64)
    }
}

/// The files under `root`, relative with `/`. Hidden folders and [`SKIPPED`] are not looked
/// into; a project past `MAX_WALK` entries is only partly listed.
pub fn files(root: &Path) -> Vec<(String, Seen)> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_WALK {
                break;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !name.starts_with('.') && !SKIPPED.contains(&name.as_ref()) {
                    stack.push(entry.path());
                }
            } else if kind.is_file() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let Ok(modified) = meta.modified() else {
                    continue;
                };
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .map(|p| {
                        p.components()
                            .map(|c| c.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("/")
                    })
                    .unwrap_or_default();
                let modified_ns = modified
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                found.push((
                    rel,
                    Seen {
                        len: meta.len(),
                        modified_ns,
                    },
                ));
            }
        }
    }
    found
}

/// Files under `root` modified at or after `since`, relative with `/`, newest first.
pub fn written_since(root: &Path, since: SystemTime) -> Vec<(String, u64)> {
    let mut found: Vec<(String, Seen)> = files(root)
        .into_iter()
        .filter(|(_, f)| f.modified() >= since)
        .collect();
    found.sort_by(|a, b| b.1.modified_ns.cmp(&a.1.modified_ns).then(a.0.cmp(&b.0)));
    found.into_iter().map(|(p, f)| (p, f.len)).collect()
}

/// The folder's files before some work, and a copy of each.
#[derive(Serialize, Deserialize)]
pub struct Before {
    files: Vec<(String, Seen)>,
    copies: PathBuf,
    copied: HashSet<String>,
}

/// What the work changed, against [`Before`].
#[derive(Default)]
pub struct Kept {
    /// New or changed files, relative with `/`, sorted.
    pub written: Vec<String>,
    /// The versions before of files changed or removed.
    pub versions: Vec<ProducedArtifact>,
    pub removed: Vec<String>,
    /// Files changed or removed whose version before was not copied (too large, past the
    /// total, or past [`MAX_VERSIONS`]).
    pub no_copy: Vec<String>,
}

impl Before {
    /// List `root` and copy its files into `copies`.
    pub fn take(root: &Path, copies: &Path) -> Before {
        let root = &canonical(root);
        let files = files(root);
        let mut copied = HashSet::new();
        let mut total = 0u64;
        for (rel, f) in &files {
            if f.len > BEFORE_FILE || total + f.len > BEFORE_TOTAL {
                continue;
            }
            let to = copies.join(rel);
            let made = to
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|_| ark_os::fs::open_capture(root, &root.join(rel)))
                .and_then(|source| {
                    ark_os::fs::copy_bounded(source, &to, BEFORE_FILE.min(BEFORE_TOTAL - total))
                });
            if let Ok(size) = made {
                total += size;
                copied.insert(rel.clone());
            }
        }
        Before {
            files,
            copies: copies.to_path_buf(),
            copied,
        }
    }

    /// Keep the listing in `path`, to compare against after a restart ([`Before::load`]).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, serde_json::to_vec(self)?)
    }

    /// A listing [`Before::save`] kept, with its copies still in place.
    pub fn load(path: &Path) -> Option<Before> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// Compare with the folder now.
    pub fn changed(&self, root: &Path) -> Kept {
        let (root, copies) = (&canonical(root), &canonical(&self.copies));
        let now: HashMap<String, Seen> = files(root).into_iter().collect();
        let was: HashMap<&str, &Seen> = self.files.iter().map(|(r, s)| (r.as_str(), s)).collect();
        let mut kept = Kept::default();
        let mut retained = 0u64;
        for (rel, f) in &now {
            if was.get(rel.as_str()).is_none_or(|w| *w != f) {
                kept.written.push(rel.clone());
            }
        }
        kept.written.sort();
        for (rel, was) in &self.files {
            let gone = match now.get(rel) {
                Some(f) if f == was => continue,
                Some(_) => false,
                None => !root.join(rel).exists(),
            };
            if gone {
                kept.removed.push(rel.clone());
            }
            let copy = copies.join(rel);
            if self.copied.contains(rel)
                && !gone
                && same_files(copies, &copy, root, &root.join(rel))
            {
                kept.written.retain(|w| w != rel);
                continue;
            }
            let bytes = self
                .copied
                .contains(rel)
                .then(|| {
                    ark_os::fs::open_capture(copies, &copy)
                        .and_then(|file| {
                            ark_os::fs::read_bounded(file, BEFORE_FILE.min(BEFORE_TOTAL - retained))
                        })
                        .ok()
                })
                .flatten();
            match bytes {
                Some(b) if kept.versions.len() < MAX_VERSIONS => {
                    retained += b.len() as u64;
                    kept.versions.push(ProducedArtifact::before(
                        ArtifactKind::Blob,
                        rel.as_str(),
                        b,
                    ));
                }
                _ => kept.no_copy.push(rel.clone()),
            }
        }
        kept
    }
}

/// Captures open files below a root without following links, so a root given through a link
/// (a project opened by a linked path) is resolved first. A missing root stays as given.
fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// Compare opened handles with fixed buffers. A file that grew is different without reading
/// its additional bytes, including when a concurrent editor changes it after the listing.
fn same_files(a_root: &Path, a: &Path, b_root: &Path, b: &Path) -> bool {
    use std::io::Read;
    let compare = || -> std::io::Result<bool> {
        let mut a = ark_os::fs::open_capture(a_root, a)?;
        let mut b = ark_os::fs::open_capture(b_root, b)?;
        let size = a.metadata()?.len();
        if size > BEFORE_FILE || size != b.metadata()?.len() {
            return Ok(false);
        }
        let mut left = size;
        let (mut ab, mut bb) = ([0; 8192], [0; 8192]);
        while left > 0 {
            let n = left.min(ab.len() as u64) as usize;
            a.read_exact(&mut ab[..n])?;
            b.read_exact(&mut bb[..n])?;
            if ab[..n] != bb[..n] {
                return Ok(false);
            }
            left -= n as u64;
        }
        Ok(a.read(&mut ab[..1])? == 0 && b.read(&mut bb[..1])? == 0)
    };
    compare().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grown_file_keeps_its_small_original_without_loading_the_grown_contents() {
        let root = tempfile::tempdir().unwrap();
        let copies = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        std::fs::write(root.join("file"), b"original").unwrap();
        let before = Before::take(&root, copies.path());
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(root.join("file"))
            .unwrap();
        file.set_len(BEFORE_FILE + 1).unwrap();
        let changed = before.changed(&root);
        assert_eq!(changed.versions.len(), 1);
        assert_eq!(changed.versions[0].bytes, b"original");
        assert_eq!(changed.written, ["file"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_project_opened_through_a_link_keeps_its_versions_before() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("file"), b"original").unwrap();
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let before = Before::take(&linked, &dir.path().join("copies"));
        std::fs::write(real.join("file"), b"changed").unwrap();
        let changed = before.changed(&linked);
        assert_eq!(changed.no_copy, Vec::<String>::new());
        assert_eq!(changed.versions.len(), 1);
        assert_eq!(changed.versions[0].bytes, b"original");
    }

    #[test]
    fn capture_rejects_an_over_limit_file_instead_of_returning_partial_bytes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file"), b"abc").unwrap();
        assert!(read_capture(root.path(), "file", 2).is_err());
        assert_eq!(read_capture(root.path(), "file", 3).unwrap(), b"abc");
    }

    #[test]
    fn new_changed_and_removed_files_survive_a_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let (root, store) = (dir.path().join("p"), dir.path().join("s"));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        std::fs::write(root.join("sub/b.md"), "b").unwrap();
        std::fs::write(root.join("same.md"), "s").unwrap();
        Before::take(&root, &store.join("copies"))
            .save(&store.join("list.json"))
            .unwrap();

        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(root.join("a.md"), "a2").unwrap();
        std::fs::remove_file(root.join("sub/b.md")).unwrap();
        std::fs::write(root.join("new.md"), "n").unwrap();
        std::fs::write(root.join("same.md"), "s").unwrap();

        let kept = Before::load(&store.join("list.json"))
            .unwrap()
            .changed(&root);
        assert_eq!(kept.written, vec!["a.md", "new.md"]);
        assert_eq!(kept.removed, vec!["sub/b.md"]);
        let mut befores: Vec<(String, Vec<u8>)> = kept
            .versions
            .iter()
            .map(|v| (v.path.clone().unwrap(), v.bytes.clone()))
            .collect();
        befores.sort();
        assert_eq!(
            befores,
            vec![
                ("a.md".into(), b"a".to_vec()),
                ("sub/b.md".into(), b"b".to_vec())
            ]
        );
        assert!(kept.no_copy.is_empty());
    }
}
