//! What contains a run, the same rules on every system: it reads the system, its runtime's
//! environment and the project folder; it writes only the project folder and its own scratch
//! folder; it has no network. Each system enforces them with its own mechanism, and nothing
//! outside this module knows which one:
//!
//! | System | Mechanism | Needs |
//! |---|---|---|
//! | macOS | a sandbox profile (`sandbox-exec`) | nothing |
//! | Linux | bubblewrap (`bwrap`): new namespaces, only the allowed folders mounted, no network | `bwrap`, user namespaces |
//! | Windows | an AppContainer: access only where its SID is granted, no network capability | [`entry`] called first in `main` |
//!
//! Where none is available [`Sandbox::native`] is `None` and ARK does not offer runs.

use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// Folders of the project a run may not write even though they are inside it: what tools run
/// or read as instructions later (git hooks, ARK's and other agents' skills, editor tasks).
pub(crate) const KEPT: &[&str] = &[
    ".git", ".ark", ".agents", ".claude", ".codex", ".vscode", ".idea", ".envrc", ".github",
];

/// What one run may touch.
#[derive(Clone, Debug)]
pub struct Bounds {
    /// The project folder: read and written.
    pub project: PathBuf,
    /// The run's own folder (its program file, its temporary files): read and written.
    pub scratch: PathBuf,
    /// Folders only read (the runtime's environment).
    pub reads: Vec<PathBuf>,
}

/// A way to contain a run on this system.
#[derive(Clone, Debug)]
pub struct Sandbox {
    /// The program that starts a contained run: `sandbox-exec`, `bwrap`, or on Windows this
    /// program itself (see [`entry`]).
    exec: PathBuf,
}

impl Sandbox {
    /// This system's sandbox, or `None` where ARK has none that works here.
    pub fn native() -> Option<Sandbox> {
        let exec = native_exec()?;
        Some(Sandbox { exec })
    }

    /// What contains runs here, for messages (`macOS sandbox`, `bubblewrap`, `AppContainer`).
    pub fn name(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macOS sandbox"
        } else if cfg!(windows) {
            "AppContainer"
        } else {
            "bubblewrap"
        }
    }

    /// The program to start and the arguments that put `program args` inside `bounds`. On
    /// Windows this also gives the run's AppContainer access to the folders in `bounds` (once
    /// per folder; the first time on a large folder takes a while), so call it off the async
    /// threads.
    pub fn wrap(
        &self,
        bounds: &Bounds,
        program: &Path,
        args: &[String],
    ) -> Result<(PathBuf, Vec<String>), String> {
        wrap_native(&self.exec, bounds, program, args)
    }
}

#[cfg(target_os = "macos")]
fn native_exec() -> Option<PathBuf> {
    macos::find()
}
#[cfg(target_os = "macos")]
fn wrap_native(
    exec: &Path,
    b: &Bounds,
    program: &Path,
    args: &[String],
) -> Result<(PathBuf, Vec<String>), String> {
    Ok(macos::wrap(exec, b, program, args))
}

#[cfg(target_os = "linux")]
fn native_exec() -> Option<PathBuf> {
    linux::find()
}
#[cfg(target_os = "linux")]
fn wrap_native(
    exec: &Path,
    b: &Bounds,
    program: &Path,
    args: &[String],
) -> Result<(PathBuf, Vec<String>), String> {
    Ok(linux::wrap(exec, b, program, args))
}

#[cfg(windows)]
fn native_exec() -> Option<PathBuf> {
    windows::find()
}
#[cfg(windows)]
fn wrap_native(
    exec: &Path,
    b: &Bounds,
    program: &Path,
    args: &[String],
) -> Result<(PathBuf, Vec<String>), String> {
    windows::wrap(exec, b, program, args)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn native_exec() -> Option<PathBuf> {
    None
}
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn wrap_native(
    _: &Path,
    _: &Bounds,
    _: &Path,
    _: &[String],
) -> Result<(PathBuf, Vec<String>), String> {
    Err("no sandbox on this system".into())
}

/// Call this first in `main` of a program that offers Run code. On Windows a contained run is
/// started by the program itself, started again with a launcher argument: then this runs the
/// program inside its AppContainer and exits with its exit code. Otherwise it returns at once,
/// and on Windows it marks this program as able to start contained runs: without the call,
/// [`Sandbox::native`] is `None` there and Run code is not offered. On macOS and Linux it does
/// nothing.
pub fn entry() {
    #[cfg(windows)]
    windows::entry();
}

/// The environment every run gets, before its runtime's own: its home and temporary folders
/// (`home`, inside the run's scratch folder), `PATH` with the runtime's folder (`bin`) first,
/// and what programs on this system expect to find.
pub(crate) fn base_env(home: &Path, bin: Option<&Path>) -> Vec<(String, String)> {
    let home = home.display().to_string();
    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut path: Vec<String> = bin.map(|b| b.display().to_string()).into_iter().collect();
    let mut env = Vec::new();
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        path.push(format!(r"{root}\System32"));
        path.push(root.clone());
        let tmp = format!(r"{home}\");
        for (k, v) in [
            ("SystemRoot", root.clone()),
            ("windir", root.clone()),
            ("ComSpec", format!(r"{root}\System32\cmd.exe")),
            ("PATHEXT", ".COM;.EXE;.BAT;.CMD".into()),
            ("USERPROFILE", home.clone()),
            ("HOME", home.clone()),
            ("APPDATA", home.clone()),
            ("LOCALAPPDATA", home.clone()),
            ("TEMP", tmp.clone()),
            ("TMP", tmp),
        ] {
            env.push((k.to_string(), v));
        }
    } else {
        path.extend(["/usr/bin".to_string(), "/bin".to_string()]);
        env.push(("HOME".into(), home.clone()));
        env.push(("TMPDIR".into(), format!("{home}/")));
        env.push(("LANG".into(), "en_US.UTF-8".into()));
    }
    env.push(("PATH".into(), path.join(sep)));
    env
}

/// The folders of [`KEPT`] that do not exist in `project`. A sandbox that can only protect
/// what exists (bubblewrap, AppContainer) lets a run create one; [`remove_created`] takes it
/// away after the run.
pub(crate) fn absent_kept(project: &Path) -> Vec<PathBuf> {
    KEPT.iter()
        .map(|k| project.join(k))
        .filter(|p| std::fs::symlink_metadata(p).is_err())
        .collect()
}

/// Remove what a run created at the paths that were absent before it ([`absent_kept`]), and
/// name them.
pub(crate) fn remove_created(absent: &[PathBuf]) -> Vec<String> {
    let mut removed = Vec::new();
    for p in absent {
        let Ok(meta) = std::fs::symlink_metadata(p) else {
            continue;
        };
        let gone = if meta.is_dir() {
            std::fs::remove_dir_all(p)
        } else {
            std::fs::remove_file(p)
        };
        if gone.is_ok() {
            if let Some(name) = p.file_name() {
                removed.push(name.to_string_lossy().into_owned());
            }
        }
    }
    removed
}

/// A path as the sandbox compares it: resolved, so `/tmp` is `/private/tmp` on macOS. On
/// Windows the `\\?\` prefix is left off, which some programs do not accept.
pub(crate) fn real(p: &Path) -> PathBuf {
    let r = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    #[cfg(windows)]
    {
        let s = r.display().to_string();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            if !rest.starts_with("UNC\\") {
                return PathBuf::from(rest);
            }
        }
    }
    r
}
