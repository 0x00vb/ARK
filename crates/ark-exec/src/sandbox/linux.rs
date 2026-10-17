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
    let mut mount = |how: &str, from: &Path, to: &Path| {
        out.extend([how.to_string(), s(from), s(to)]);
    };
    for p in SYSTEM {
        mount("--ro-bind-try", Path::new(p), Path::new(p));
    }
    for r in b.only_reads() {
        let at = real(r);
        mount("--ro-bind-try", &at, &at);
        // A read reached through a link (uv's `cpython-3.12-…` names the newest 3.12.x, and
        // an environment's `python` points through it) is also where the link says.
        if r.is_absolute() && *r != at {
            mount("--ro-bind-try", &at, r);
        }
    }
    let project = real(&b.project);
    mount("--bind", &project, &project);
    let scratch = real(&b.scratch);
    mount("--bind", &scratch, &scratch);
    for k in KEPT {
        let kept = project.join(k);
        mount("--ro-bind-try", &kept, &kept);
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
        let b = Bounds::run(
            "/work/p".into(),
            "/store/runs/1".into(),
            vec!["/store/python".into()],
        );
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

    #[test]
    fn a_read_through_a_link_is_there_under_both_names() {
        let dir = std::env::temp_dir().join(format!("ark-bwrap-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("cpython-3.12.11")).unwrap();
        std::os::unix::fs::symlink(dir.join("cpython-3.12.11"), dir.join("cpython-3.12")).unwrap();
        let b = Bounds::run(
            "/work/p".into(),
            "/store/runs/1".into(),
            vec![dir.join("cpython-3.12")],
        );
        let (_, a) = wrap(Path::new("/usr/bin/bwrap"), &b, Path::new("/py"), &[]);
        let line = a.join(" ");
        let target = real(&dir.join("cpython-3.12.11")).display().to_string();
        assert!(
            line.contains(&format!("--ro-bind-try {target} {target}")),
            "{line}"
        );
        let link = dir.join("cpython-3.12").display().to_string();
        assert!(
            line.contains(&format!("--ro-bind-try {target} {link}")),
            "{line}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
