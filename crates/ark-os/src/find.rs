//! Finding programs and installed apps.
//!
//! A GUI app starts with a short `PATH` (macOS gives it `/usr/bin:/bin:/usr/sbin:/sbin`), so
//! after `PATH` the places installers put programs are searched too. On Windows a bare name
//! is tried with each extension in `PATHEXT` (`claude` finds `claude.exe` or `claude.cmd`),
//! and a file without one (npm's shell script next to its `.cmd`) is never picked.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The program `name`, or `None`. A path (`./bin/tool`, `C:\tools\x.exe`) is checked as given.
pub fn program(name: &str) -> Option<PathBuf> {
    program_in(name, Vec::new())
}

/// [`program`], also looking in `extra` after `PATH` and before the usual install places.
pub fn program_in(name: &str, extra: Vec<PathBuf>) -> Option<PathBuf> {
    let given = Path::new(name);
    if given.is_absolute() || given.components().count() > 1 {
        return names(given.as_os_str(), pathext().as_deref())
            .into_iter()
            .map(PathBuf::from)
            .find(|p| runnable(p));
    }
    let names = names(given.as_os_str(), pathext().as_deref());
    path_dirs()
        .into_iter()
        .chain(extra)
        .chain(usual_places())
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| runnable(p))
}

/// An installed desktop app, by where its installer puts it on each system.
#[derive(Clone, Copy, Debug, Default)]
pub struct App<'a> {
    /// macOS: the executable inside `/Applications` or `~/Applications`, such as
    /// `Google Chrome.app/Contents/MacOS/Google Chrome`.
    pub macos: &'a [&'a str],
    /// Windows: the executable under Program Files, Program Files (x86) or the local app data
    /// folder, such as `Google/Chrome/Application/chrome.exe`.
    pub windows: &'a [&'a str],
    /// Elsewhere: program names, found like [`program`].
    pub other: &'a [&'a str],
}

/// The first of `app`'s executables that is installed.
pub fn app(app: &App) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let mut roots = vec![PathBuf::from("/Applications")];
        roots.extend(crate::dirs::home().map(|h| h.join("Applications")));
        app.macos
            .iter()
            .flat_map(|rel| roots.iter().map(move |r| r.join(rel)))
            .find(|p| p.is_file())
    } else if cfg!(windows) {
        let roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
            .iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .collect();
        app.windows
            .iter()
            .flat_map(|rel| roots.iter().map(move |r| r.join(rel)))
            .find(|p| p.is_file())
    } else {
        app.other.iter().find_map(|n| program(n))
    }
}

/// `PATHEXT` on Windows (with its default when unset); `None` elsewhere.
fn pathext() -> Option<String> {
    cfg!(windows).then(|| {
        std::env::var("PATHEXT")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
    })
}

/// The file names `name` may have: itself on Unix; with each `PATHEXT` extension on Windows,
/// or itself when it already ends in one.
fn names(name: &OsStr, pathext: Option<&str>) -> Vec<OsString> {
    let Some(pathext) = pathext else {
        return vec![name.to_os_string()];
    };
    let exts: Vec<&str> = pathext
        .split(';')
        .map(str::trim)
        .filter(|e| e.starts_with('.'))
        .collect();
    let lower = name.to_string_lossy().to_ascii_lowercase();
    if exts
        .iter()
        .any(|e| lower.ends_with(&e.to_ascii_lowercase()))
    {
        return vec![name.to_os_string()];
    }
    exts.iter()
        .map(|e| {
            let mut n = name.to_os_string();
            n.push(e.to_ascii_lowercase());
            n
        })
        .collect()
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

/// Where installers put programs that a short `PATH` misses.
fn usual_places() -> Vec<PathBuf> {
    let home = crate::dirs::home();
    let mut dirs = Vec::new();
    if cfg!(windows) {
        let env = |v: &str| std::env::var_os(v).map(PathBuf::from);
        if let Some(h) = &home {
            // Claude Code's native installer, then scoop, bun and cargo.
            for d in [".local/bin", "scoop/shims", ".bun/bin", ".cargo/bin"] {
                dirs.push(h.join(d));
            }
        }
        dirs.extend(env("APPDATA").map(|a| a.join("npm")));
        if let Some(l) = env("LOCALAPPDATA") {
            dirs.push(l.join("Microsoft/WinGet/Links"));
            dirs.push(l.join("Volta/bin"));
        }
        dirs.extend(env("NVM_SYMLINK"));
        dirs.extend(env("ProgramFiles").map(|p| p.join("nodejs")));
    } else {
        if let Some(h) = &home {
            for d in [
                ".local/bin",
                ".npm-global/bin",
                ".bun/bin",
                ".volta/bin",
                ".cargo/bin",
            ] {
                dirs.push(h.join(d));
            }
            // nvm keeps one bin folder per Node version: newest first.
            if let Ok(rd) = std::fs::read_dir(h.join(".nvm/versions/node")) {
                let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).collect();
                v.sort();
                dirs.extend(v.into_iter().rev());
            }
        }
        for d in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
            dirs.push(PathBuf::from(d));
        }
    }
    dirs
}

/// A file this system would run: executable on Unix, any file on Windows (its name already
/// carries a `PATHEXT` extension).
fn runnable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_tries_each_extension_and_never_a_bare_name() {
        let ext = Some(".COM;.EXE;.BAT;.CMD");
        let n = names(OsStr::new("claude"), ext);
        assert_eq!(n, ["claude.com", "claude.exe", "claude.bat", "claude.cmd"]);
        assert_eq!(names(OsStr::new("node.EXE"), ext), ["node.EXE"]);
        assert_eq!(names(OsStr::new("claude"), None), ["claude"]);
        // Empty entries and junk in PATHEXT are skipped.
        assert_eq!(names(OsStr::new("x"), Some(";.EXE; ;nope")), ["x.exe"]);
    }

    #[cfg(unix)]
    #[test]
    fn finds_an_executable_in_extra_folders_and_skips_one_that_is_not() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let name = format!("ark-os-find-{}", std::process::id());
        let p = dir.path().join(&name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        assert_eq!(program_in(&name, vec![dir.path().into()]), None);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(program_in(&name, vec![dir.path().into()]), Some(p.clone()));
        assert_eq!(program(p.to_str().unwrap()), Some(p));
        assert!(program("sh").is_some());
    }

    #[cfg(windows)]
    #[test]
    fn finds_a_cmd_shim_by_its_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("ark-os-find-{}", std::process::id());
        std::fs::write(dir.path().join(&name), "#!/bin/sh\n").unwrap();
        let cmd = dir.path().join(format!("{name}.cmd"));
        std::fs::write(&cmd, "@echo off\r\n").unwrap();
        assert_eq!(program_in(&name, vec![dir.path().into()]), Some(cmd));
        assert!(program("cmd").is_some());
    }
}
