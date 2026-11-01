//! One runtime process writes a store (ADR 0062). Opening a store takes an exclusive OS lock on
//! `<root>/runtime.lock`. The kernel lets it go when the process ends, also on a crash, so a
//! reused pid is never taken for a live writer. Every handle on the same store in one process
//! shares the lock; a second process gets [`StoreError::Busy`].

use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::{Result, StoreError};

/// The lock file in a store's root.
pub const LOCK_FILE: &str = "runtime.lock";

/// Held while any handle of this process has the store open.
pub(crate) struct WriterLock {
    _file: File,
}

fn held() -> &'static Mutex<HashMap<PathBuf, Weak<WriterLock>>> {
    static HELD: OnceLock<Mutex<HashMap<PathBuf, Weak<WriterLock>>>> = OnceLock::new();
    HELD.get_or_init(Default::default)
}

/// The lock on the store at `root` (an existing directory), shared with this process's other
/// handles on it.
pub(crate) fn acquire(root: &Path) -> Result<Arc<WriterLock>> {
    let key = std::fs::canonicalize(root)?;
    let mut held = held().lock().unwrap();
    if let Some(lock) = held.get(&key).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(key.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            // The writer's pid, when the platform lets a locked file be read (not Windows).
            let mut text = String::new();
            let pid = file
                .read_to_string(&mut text)
                .ok()
                .and_then(|_| text.trim().parse().ok());
            return Err(StoreError::Busy { root: key, pid });
        }
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }
    file.set_len(0)?;
    file.rewind()?;
    write!(file, "{}", std::process::id())?;
    file.flush()?;
    let lock = Arc::new(WriterLock { _file: file });
    held.retain(|_, w| w.strong_count() > 0);
    held.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

#[cfg(test)]
mod tests {
    use std::io::BufRead;
    use std::process::{Command, Stdio};

    use crate::{Store, StoreError};

    const CHILD: &str = "ARK_STORE_WRITER_CHILD";

    /// The child's part of [`a_second_process_is_refused_until_the_writer_ends`]: open the store,
    /// say so, and hold it until the parent closes stdin or kills it.
    #[test]
    fn child_holds_the_store() {
        let Ok(dir) = std::env::var(CHILD) else {
            return;
        };
        let _store = Store::open(&dir).unwrap();
        println!("READY");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }

    #[test]
    fn handles_in_one_process_share_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let a = Store::open(dir.path()).unwrap();
        // Another spelling of the same folder is the same store.
        let b = Store::open(dir.path().join(".")).unwrap();
        a.put_record("k", "1", "", &serde_json::json!({"n": 1}))
            .unwrap();
        assert!(b.get_record("k", "1").unwrap().is_some());
        drop((a, b));
        Store::open(dir.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn alias_and_last_handle_keep_the_same_lock() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let alias = dir.path().join("alias");
        let first = Store::open(&root).unwrap();
        symlink(&root, &alias).unwrap();
        let second = Store::open(&alias).unwrap();
        drop(first);
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "writer::tests::child_holds_the_store",
                "--nocapture",
            ])
            .env(CHILD, &alias)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success());
        drop(second);
        Store::open(&root).unwrap();
    }

    #[test]
    fn a_second_process_is_refused_until_the_writer_ends() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "writer::tests::child_holds_the_store",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        while !line.contains("READY") {
            line.clear();
            assert!(
                out.read_line(&mut line).unwrap() > 0,
                "the child never got ready"
            );
        }
        match Store::open(dir.path()) {
            Err(StoreError::Busy { pid, .. }) => {
                if cfg!(unix) {
                    assert_eq!(pid, Some(child.id()));
                }
            }
            Err(e) => panic!("refused for another reason: {e}"),
            Ok(_) => panic!("a second writer opened the store"),
        }
        let e = Store::open(dir.path()).err().unwrap().to_string();
        assert!(e.contains("stop the other writer"), "{e}");
        // A crash frees it: the kernel drops the lock with the process.
        child.kill().unwrap();
        child.wait().unwrap();
        Store::open(dir.path()).unwrap();
    }
}
