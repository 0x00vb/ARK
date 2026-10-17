//! What contains a run, the same rules on every system: it reads the system, its runtime's
//! environment and the project folder; it writes only the project folder and its own scratch
//! folder; it has no network. Each system enforces them with its own mechanism, and nothing
//! outside this module knows which one. [`Bounds::run`] are those bounds (`code.run`); an agent
//! program ARK starts gets wider ones, on macOS only for now (ADR 0074, [`Sandbox::programs`]):
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

/// Credential folders in a person's home that an agent program never reads, even inside its
/// project (ADR 0074).
pub const PRIVATE: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".netrc",
    ".git-credentials",
    ".config/gcloud",
    ".config/gh",
];

/// What one run may touch. [`Bounds::run`] builds `code.run`'s, which nothing else widens.
#[derive(Clone, Debug)]
pub struct Bounds {
    /// The project folder: read and written, except its kept folders.
    pub project: PathBuf,
    /// The run's own folder (its program file, its temporary files): read and written.
    pub scratch: PathBuf,
    /// What it reads besides the project and the scratch folder.
    pub reads: Reads,
    /// Where it writes besides the project and the scratch folder (a program's own state).
    pub writes: Vec<Write>,
    /// Names inside each folder of `writes` that stay read-only: what runs later outside the
    /// sandbox (a program's own files, settings with hooks).
    pub kept: Vec<String>,
    pub network: Network,
    /// macOS: it may ask the person's keychain (a program whose login is kept there).
    pub keychain: bool,
}

/// What a run reads besides its project and scratch folder.
#[derive(Clone, Debug)]
pub enum Reads {
    /// The system's folders and these (`code.run`: its runtime's environment).
    Only(Vec<PathBuf>),
    /// Everything but `hidden`, where only `allowed` is read again; `never` is not read even
    /// inside the project, only where `writes` names it.
    Except {
        hidden: Vec<PathBuf>,
        allowed: Vec<PathBuf>,
        never: Vec<PathBuf>,
    },
}

/// A place a run writes besides its project and scratch folder.
#[derive(Clone, Debug)]
pub enum Write {
    Folder(PathBuf),
    /// The file, and its siblings named after it and a dot (`.lock`, a temporary copy).
    File(PathBuf),
}

/// The network a run reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    None,
    /// Outbound TCP to port 443 and name lookups. A sandbox cannot tell one host from another.
    Https,
}

impl Bounds {
    /// `code.run`'s bounds: the system and `reads` read, the project and `scratch` written, no
    /// network.
    pub fn run(project: PathBuf, scratch: PathBuf, reads: Vec<PathBuf>) -> Bounds {
        Bounds {
            project,
            scratch,
            reads: Reads::Only(reads),
            writes: Vec::new(),
            kept: Vec::new(),
            network: Network::None,
            keychain: false,
        }
    }

    /// Whether these are no wider than [`Bounds::run`]'s.
    pub(crate) fn is_run(&self) -> bool {
        matches!(self.reads, Reads::Only(_))
            && self.writes.is_empty()
            && self.network == Network::None
            && !self.keychain
    }

    /// The read folders of [`Reads::Only`]; empty for anything wider.
    pub(crate) fn only_reads(&self) -> &[PathBuf] {
        match &self.reads {
            Reads::Only(r) => r,
            Reads::Except { .. } => &[],
        }
    }
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

    /// This system's sandbox for the agent programs ARK starts (ADR 0074): macOS only for now.
    /// Linux and Windows contain `code.run` but not yet a program with its own state and network.
    pub fn programs() -> Option<Sandbox> {
        if cfg!(target_os = "macos") {
            Sandbox::native()
        } else {
            None
        }
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
    if !b.is_run() {
        return Err(NOT_YET.into());
    }
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
    if !b.is_run() {
        return Err(NOT_YET.into());
    }
    windows::wrap(exec, b, program, args)
}

#[cfg(any(target_os = "linux", windows))]
const NOT_YET: &str = "this system does not contain a program with its own state and network yet";

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

#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests {
    use super::*;

    #[test]
    fn wider_bounds_than_code_runs_are_refused_here() {
        let mut b = Bounds::run("/work/p".into(), "/run/1".into(), vec![]);
        b.network = Network::Https;
        assert!(wrap_native(Path::new("/x"), &b, Path::new("/bin/sh"), &[]).is_err());
        assert!(Sandbox::programs().is_none());
    }
}
