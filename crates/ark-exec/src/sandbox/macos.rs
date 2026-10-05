//! macOS: a sandbox profile run by `sandbox-exec`. Paths go in as parameters, never into the
//! profile text, so no folder name can change the rules.

use std::path::{Path, PathBuf};

use super::{real, Bounds, KEPT};

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

pub(super) fn find() -> Option<PathBuf> {
    let exec = PathBuf::from("/usr/bin/sandbox-exec");
    exec.is_file().then_some(exec)
}

pub(super) fn wrap(
    exec: &Path,
    bounds: &Bounds,
    program: &Path,
    args: &[String],
) -> (PathBuf, Vec<String>) {
    let (profile, params) = profile(bounds);
    let mut out = vec!["-p".to_string(), profile];
    for (k, v) in params {
        out.push("-D".into());
        out.push(format!("{k}={v}"));
    }
    out.push(program.display().to_string());
    out.extend(args.iter().cloned());
    (exec.to_path_buf(), out)
}

/// The profile and its parameters. Paths go in as parameters, never into the profile text, so
/// no folder name can change the rules. The sandbox compares real paths, so every path is
/// resolved first (`/tmp` is `/private/tmp`).
fn profile(b: &Bounds) -> (String, Vec<(String, String)>) {
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
