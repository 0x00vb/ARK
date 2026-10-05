//! Linux: bubblewrap (`bwrap`). A run starts in new namespaces (no network among them) with an
//! empty root where only the system folders, its environment and the project are mounted: the
//! system and the environment read-only, the project and the run's scratch folder writable, the
//! project's kept folders read-only again on top. A folder that is not mounted does not exist
//! for the run.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{real, Bounds, KEPT};

/// System folders every program needs to read. Those missing on a system are skipped.
const SYSTEM: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/lib32",
    "/etc",
    "/opt",
    "/nix/store",
];

/// `bwrap` on `PATH` or in `/usr/bin`, when it can start a run here: user namespaces may be
/// turned off (some containers), and then ARK does not offer runs.
pub(super) fn find() -> Option<PathBuf> {
    let exec = std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("bwrap"))
                .find(|p| p.is_file())
        })
        .or_else(|| Some(PathBuf::from("/usr/bin/bwrap")).filter(|p| p.is_file()))?;
    let works = Command::new(&exec)
        .args([
            "--unshare-all",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--",
            "/bin/sh",
            "-c",
            ":",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !works {
        tracing::warn!(
            "{} cannot start a sandbox here (user namespaces off?); Run code is not offered",
            exec.display()
        );
    }
    works.then_some(exec)
}

pub(super) fn wrap(
    exec: &Path,
    b: &Bounds,
    program: &Path,
    args: &[String],
) -> (PathBuf, Vec<String>) {
    let s = |p: &Path| p.display().to_string();
    let mut out: Vec<String> = [
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();
    let mut bind = |how: &str, p: &Path| {
        out.push(how.to_string());
        out.push(s(p));
        out.push(s(p));
    };
    for p in SYSTEM {
        bind("--ro-bind-try", Path::new(p));
    }
    for r in &b.reads {
        bind("--ro-bind-try", &real(r));
    }
    let project = real(&b.project);
    bind("--bind", &project);
    bind("--bind", &real(&b.scratch));
    for k in KEPT {
        bind("--ro-bind-try", &project.join(k));
    }
    out.extend(["--chdir".to_string(), s(&project), "--".to_string()]);
    out.push(s(program));
    out.extend(args.iter().cloned());
    (exec.to_path_buf(), out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_is_writable_and_its_kept_folders_are_not() {
        let b = Bounds {
            project: "/work/p".into(),
            scratch: "/store/runs/1".into(),
            reads: vec!["/store/python".into()],
        };
        let (_, a) = wrap(
            Path::new("/usr/bin/bwrap"),
            &b,
            Path::new("/py"),
            &["x.py".into()],
        );
        let line = a.join(" ");
        assert!(line.starts_with("--unshare-all"), "{line}");
        assert!(line.contains("--bind /work/p /work/p"), "{line}");
        assert!(
            line.contains("--ro-bind-try /work/p/.git /work/p/.git"),
            "{line}"
        );
        assert!(
            line.contains("--ro-bind-try /store/python /store/python"),
            "{line}"
        );
        // The kept folders come after the project, so they cover it.
        assert!(line.find("--bind /work/p ").unwrap() < line.find("/work/p/.git").unwrap());
        assert!(line.ends_with("--chdir /work/p -- /py x.py"), "{line}");
        assert!(!line.contains("--share-net"));
    }
}
