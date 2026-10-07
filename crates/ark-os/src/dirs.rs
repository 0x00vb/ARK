//! Folders. Never read `HOME`: Windows has no such variable, and a GUI app may start without it.

use std::path::{Path, PathBuf};

/// The person's home folder: `$HOME` on Unix, the profile folder on Windows.
pub fn home() -> Option<PathBuf> {
    ::dirs::home_dir()
}

/// Where an app keeps its data on this machine. Unix keeps ARK's convention, a dot folder in
/// the home (`~/.ark-desktop`), so existing installs keep their data. Windows uses the local
/// (not roaming) app data folder, `%LOCALAPPDATA%\<name>`: stores and caches are large and
/// belong to this machine.
pub fn app_home(dot_name: &str, name: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        ::dirs::data_local_dir().map(|d| d.join(name))
    } else {
        home().map(|h| h.join(dot_name))
    }
}

/// The Documents folder. On Windows it follows the person's setting, including a Documents
/// folder moved to OneDrive.
pub fn documents() -> Option<PathBuf> {
    ::dirs::document_dir().or_else(|| home().map(|h| h.join("Documents")))
}

/// `std::fs::canonicalize` without Windows' `\\?\` prefix when the path does not need it.
/// The prefixed form confuses people when shown, other programs as a working folder, and
/// any code that compares or splits paths as text.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    dunce::canonicalize(path)
}

/// `path` without a `\\?\` prefix it does not need. Unchanged elsewhere.
pub fn simplified(path: &Path) -> &Path {
    dunce::simplified(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_resolve() {
        assert!(home().is_some());
        let app = app_home(".ark-test", "ARK Test").unwrap();
        if cfg!(windows) {
            assert!(app.ends_with("ARK Test"));
        } else {
            assert_eq!(app, home().unwrap().join(".ark-test"));
        }
        assert!(documents().is_some());
    }

    #[test]
    fn canonical_paths_read_plainly() {
        let dir = tempfile::tempdir().unwrap();
        let p = canonicalize(dir.path()).unwrap();
        assert!(!p.to_string_lossy().starts_with(r"\\?\"), "{}", p.display());
        assert_eq!(simplified(&p), p.as_path());
    }
}
