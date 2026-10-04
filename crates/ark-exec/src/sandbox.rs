//! What contains a run: it reads the system, its runtime's environment and the project folder;
//! it writes only the project folder and its own scratch folder; it has no network. macOS
//! enforces this with a sandbox profile (`sandbox-exec`). Another platform needs its own
//! mechanism before ARK offers runs there.

use std::path::{Path, PathBuf};

/// Folders of the project a run may not write even though they are inside it: what tools run
/// or read as instructions later (git hooks, ARK's and other agents' skills, editor tasks).
const KEPT: &[&str] = &[
    ".git", ".ark", ".agents", ".claude", ".codex", ".vscode", ".idea", ".envrc", ".github",
];

/// System folders every program needs to read (libraries, frameworks, devices, settings).
const SYSTEM: &[&str] = &[
    "/usr",
    "/System",
    "/Library",
    "/bin",
    "/sbin",
    "/opt/homebrew",
    "/private/etc",
    "/dev",
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

/// A way to contain a run on this platform.
#[derive(Clone, Debug)]
pub struct Sandbox {
    exec: PathBuf,
}

impl Sandbox {
    /// This platform's sandbox, or `None` where ARK has none (every platform but macOS today).
    pub fn native() -> Option<Sandbox> {
        let exec = PathBuf::from("/usr/bin/sandbox-exec");
        (cfg!(target_os = "macos") && exec.is_file()).then_some(Sandbox { exec })
    }

    /// The program to start and the arguments that put `program args` inside `bounds`.
    pub fn wrap(&self, bounds: &Bounds, program: &Path, args: &[String]) -> (PathBuf, Vec<String>) {
        let (profile, params) = profile(bounds);
        let mut out = vec!["-p".to_string(), profile];
        for (k, v) in params {
            out.push("-D".into());
            out.push(format!("{k}={v}"));
        }
        out.push(program.display().to_string());
        out.extend(args.iter().cloned());
        (self.exec.clone(), out)
    }
}

/// The profile and its parameters. Paths go in as parameters, never into the profile text, so
/// no folder name can change the rules. The sandbox compares real paths, so every path is
/// resolved first (`/tmp` is `/private/tmp`).
fn profile(b: &Bounds) -> (String, Vec<(String, String)>) {
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let mut params = Vec::new();
    let mut param = |name: String, p: &Path| {
        params.push((name.clone(), real(p).display().to_string()));
        format!("(param \"{name}\")")
    };
    let project = param("PROJECT".into(), &b.project);
    let scratch = param("SCRATCH".into(), &b.scratch);
    let reads: Vec<String> = b
        .reads
        .iter()
        .enumerate()
        .map(|(i, p)| format!("(subpath {})", param(format!("READ{i}"), p)))
        .collect();
    let kept: Vec<String> = KEPT
        .iter()
        .enumerate()
        .map(|(i, name)| {
            format!(
                "(subpath {})",
                param(format!("KEPT{i}"), &real(&b.project).join(name))
            )
        })
        .collect();
    let system: Vec<String> = SYSTEM
        .iter()
        .map(|p| format!("(subpath \"{p}\")"))
        .collect();
    let text = format!(
        r#"(version 1)
(deny default)
(allow process-fork)
(allow process-exec)
(allow signal (target same-sandbox))
(allow sysctl-read)
(allow mach-lookup (global-name "com.apple.system.opendirectoryd.libinfo"))
(allow ipc-posix-shm-read-data ipc-posix-shm-write-data ipc-posix-shm-write-create)
(allow file-read-metadata)
(allow file-read* {system} (literal "/") (literal "/private") (literal "/etc") (literal "/tmp") (literal "/var")
  (subpath {project}) (subpath {scratch}) {reads})
(allow file-write* (subpath {project}) (subpath {scratch}) (literal "/dev/null"))
(deny file-write* {kept})
(deny network*)
"#,
        system = system.join(" "),
        reads = reads.join(" "),
        kept = kept.join(" "),
    );
    (text, params)
}
