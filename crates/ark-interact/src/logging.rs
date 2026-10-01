//! Logs for a running ARK application: structured, cheap, and free of conversation content.
//!
//! The runtime logs model calls, retries, provider errors, stalls, failed tool calls and one
//! summary line per turn, as fields (counts, names, durations), never prompts, answers, tool
//! inputs or outputs. `ARK_LOG` sets the level and filter (`warn` by default; `info` adds the
//! per-turn summary; `debug` adds every model call). Logs go to a file in the store for the
//! interactive shell and the workspace, and to stderr for a headless run.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where logs go.
pub enum LogTarget {
    Stderr,
    /// Appended to this file; when it is over [`MAX_LOG_BYTES`] at start it is moved to
    /// `<file>.1` (replacing the previous one) and a new file begins.
    File(PathBuf),
}

/// Size at which the log file is rotated at start.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Install the process-wide logger once. Returns false when one was already installed (by
/// the application, or an earlier call), which is not an error.
pub fn init(target: LogTarget) -> bool {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("ARK_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true);
    match target {
        LogTarget::Stderr => builder.with_writer(std::io::stderr).try_init().is_ok(),
        LogTarget::File(path) => match open_rotated(&path) {
            Ok(f) => builder
                .with_ansi(false)
                .with_writer(Mutex::new(f))
                .try_init()
                .is_ok(),
            Err(_) => false,
        },
    }
}

/// The log file for a store root.
pub fn file_in(store_root: &Path) -> PathBuf {
    store_root.join("ark.log")
}

fn open_rotated(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path)
        .map(|m| m.len() > MAX_LOG_BYTES)
        .unwrap_or(false)
    {
        let old = path.with_extension("log.1");
        let _ = std::fs::rename(path, old);
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let _ = f.flush();
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_big_log_is_rotated_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = file_in(dir.path());
        std::fs::write(&p, vec![b'x'; (MAX_LOG_BYTES + 1) as usize]).unwrap();
        let f = open_rotated(&p).unwrap();
        drop(f);
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 0, "a new file begins");
        assert!(
            p.with_extension("log.1").exists(),
            "the old one is kept once"
        );
    }
}
