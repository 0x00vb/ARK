//! macOS: a sandbox profile run by `sandbox-exec`. Paths go in as parameters, never into the
//! profile text, so no folder name can change the rules.

use std::path::{Path, PathBuf};

use super::{real, Bounds, Network, Reads, Write, KEPT};

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

/// The profile and its parameters: `code.run`'s, or an agent program's when the bounds are wider.
fn profile(b: &Bounds) -> (String, Vec<(String, String)>) {
    if b.is_run() {
        run_profile(b)
    } else {
        program_profile(b)
    }
}

/// `code.run`'s profile and its parameters. Paths go in as parameters, never into the profile
/// text, so no folder name can change the rules. The sandbox compares real paths, so every path
/// is resolved first (`/tmp` is `/private/tmp`).
fn run_profile(b: &Bounds) -> (String, Vec<(String, String)>) {
    let mut params = Vec::new();
    let mut param = |name: String, p: &Path| {
        params.push((name.clone(), real(p).display().to_string()));
        format!("(param \"{name}\")")
    };
    let project = param("PROJECT".into(), &b.project);
    let scratch = param("SCRATCH".into(), &b.scratch);
    let reads: Vec<String> = b
        .only_reads()
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

/// Profile parameters, named in order.
#[derive(Default)]
struct Params(Vec<(String, String)>);

impl Params {
    /// `p` as the parameter `name`, as the profile names it.
    fn add(&mut self, name: String, p: &Path) -> String {
        self.0.push((name.clone(), p.display().to_string()));
        format!("(param \"{name}\")")
    }

    /// Each of `list`, resolved, as a `subpath` filter.
    fn subpaths(&mut self, prefix: &str, list: &[PathBuf]) -> String {
        list.iter()
            .enumerate()
            .map(|(i, p)| format!("(subpath {})", self.add(format!("{prefix}{i}"), &real(p))))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// An agent program's profile (ADR 0074). Later rules win: reads open, then the hidden folders
/// closed, then the project, the scratch folder and what is allowed opened again; `never` closed
/// even there; the program's own state opened; kept folders closed for writes. Network and the
/// keychain only as the bounds say. A rule whose list is empty is left out (an empty filter
/// would match everything).
fn program_profile(b: &Bounds) -> (String, Vec<(String, String)>) {
    let mut params = Params::default();
    let project = params.add("PROJECT".into(), &real(&b.project));
    let scratch = params.add("SCRATCH".into(), &real(&b.scratch));
    let rule = |what: &str, list: &str| {
        if list.is_empty() {
            String::new()
        } else {
            format!("({what} {list})\n")
        }
    };
    let (reads, never) = match &b.reads {
        Reads::Only(reads) => {
            let system: Vec<String> = SYSTEM
                .iter()
                .map(|p| format!("(subpath \"{p}\")"))
                .collect();
            let reads = params.subpaths("READ", reads);
            (
                format!(
                    "(allow file-read* {} (literal \"/\") (literal \"/private\") (literal \"/etc\") (literal \"/tmp\") (literal \"/var\") (subpath {project}) (subpath {scratch}) {reads})\n",
                    system.join(" ")
                ),
                String::new(),
            )
        }
        Reads::Except {
            hidden,
            allowed,
            never,
        } => {
            let hidden = params.subpaths("HIDE", hidden);
            let allowed = params.subpaths("ALLOW", allowed);
            let never = params.subpaths("NEVER", never);
            (
                format!(
                    "(allow file-read*)\n{}(allow file-read* (subpath {project}) (subpath {scratch}) {allowed})\n",
                    rule("deny file-read*", &hidden)
                ),
                rule("deny file-read* file-write*", &never),
            )
        }
    };
    let mut writes = Vec::new();
    let mut folders = Vec::new();
    for (i, w) in b.writes.iter().enumerate() {
        match w {
            Write::Folder(f) => {
                let f = real(f);
                writes.push(format!("(subpath {})", params.add(format!("WRITE{i}"), &f)));
                folders.push(f);
            }
            Write::File(f) => {
                // Where the file is named and where it leads: a link's lock sits beside the link.
                let named = match (f.parent(), f.file_name()) {
                    (Some(dir), Some(name)) => real(dir).join(name),
                    _ => f.clone(),
                };
                let target = real(f);
                let mut at = vec![named.clone()];
                if target != named {
                    at.push(target);
                }
                for (j, path) in at.iter().enumerate() {
                    let p = params.add(format!("FILE{i}_{j}"), path);
                    writes.push(format!("(literal {p}) (prefix (string-append {p} \".\"))"));
                }
            }
        }
    }
    let project_root = real(&b.project);
    let mut kept: Vec<PathBuf> = KEPT.iter().map(|k| project_root.join(k)).collect();
    for f in &folders {
        kept.extend(b.kept.iter().map(|k| f.join(k)));
    }
    let kept = params.subpaths("KEPT", &kept);
    let network = match b.network {
        Network::None => "(deny network*)\n",
        Network::Https => concat!(
            "(allow system-socket)\n",
            "(allow network-outbound (remote tcp \"*:443\"))\n",
            "(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n",
            "(allow mach-lookup (global-name \"com.apple.SystemConfiguration.DNSConfiguration\") ",
            "(global-name \"com.apple.SystemConfiguration.configd\") ",
            "(global-name \"com.apple.trustd.agent\"))\n",
        ),
    };
    let keychain = if b.keychain {
        "(allow mach-lookup (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.securityd.xpc\"))\n"
    } else {
        ""
    };
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
{reads}(allow file-write* (subpath {project}) (subpath {scratch}) (literal "/dev/null"))
{never}{writes}(deny file-write* {kept})
{network}{keychain}"#,
        writes = rule("allow file-read* file-write*", &writes.join(" ")),
    );
    (text, params.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `code.run`'s profile, as on `main` at 990adf3 before ADR 0074: the agent-program rules
    /// must never reach it.
    #[test]
    fn code_run_keeps_its_profile_exactly() {
        let b = Bounds::run(
            "/work/p".into(),
            "/store/s".into(),
            vec!["/store/py".into()],
        );
        let (text, params) = profile(&b);
        assert_eq!(
            text,
            r#"(version 1)
(deny default)
(allow process-fork)
(allow process-exec)
(allow signal (target same-sandbox))
(allow sysctl-read)
(allow mach-lookup (global-name "com.apple.system.opendirectoryd.libinfo"))
(allow ipc-posix-shm-read-data ipc-posix-shm-write-data ipc-posix-shm-write-create)
(allow file-read-metadata)
(allow file-read* (subpath "/usr") (subpath "/System") (subpath "/Library") (subpath "/bin") (subpath "/sbin") (subpath "/opt/homebrew") (subpath "/private/etc") (subpath "/dev") (literal "/") (literal "/private") (literal "/etc") (literal "/tmp") (literal "/var")
  (subpath (param "PROJECT")) (subpath (param "SCRATCH")) (subpath (param "READ0")))
(allow file-write* (subpath (param "PROJECT")) (subpath (param "SCRATCH")) (literal "/dev/null"))
(deny file-write* (subpath (param "KEPT0")) (subpath (param "KEPT1")) (subpath (param "KEPT2")) (subpath (param "KEPT3")) (subpath (param "KEPT4")) (subpath (param "KEPT5")) (subpath (param "KEPT6")) (subpath (param "KEPT7")) (subpath (param "KEPT8")))
(deny network*)
"#
        );
        let names: Vec<&str> = params.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names[..3], ["PROJECT", "SCRATCH", "READ0"]);
        assert_eq!(params.len(), 3 + KEPT.len());
        assert_eq!(params[3].1, "/work/p/.git");
    }

    #[test]
    fn a_program_profile_orders_its_rules_and_leaves_out_empty_ones() {
        let mut b = Bounds::run("/work/p".into(), "/run/1".into(), vec![]);
        b.reads = Reads::Except {
            hidden: vec!["/home/a".into()],
            allowed: vec!["/home/a/.gitconfig".into()],
            never: vec![],
        };
        b.writes = vec![
            Write::Folder("/home/a/.claude".into()),
            Write::File("/home/a/.claude.json".into()),
        ];
        b.kept = vec!["settings.json".into()];
        b.network = Network::Https;
        let (text, params) = profile(&b);
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle}:\n{text}"))
        };
        assert!(at("(allow file-read*)\n") < at("(deny file-read* (subpath (param \"HIDE0\")))"));
        assert!(at("HIDE0") < at("(subpath (param \"ALLOW0\"))"));
        assert!(at("ALLOW0") < at("(allow file-read* file-write* (subpath (param \"WRITE0\"))"));
        assert!(at("WRITE0") < at("(deny file-write* (subpath (param \"KEPT0\"))"));
        assert!(text.contains(
            r#"(literal (param "FILE1_0")) (prefix (string-append (param "FILE1_0") "."))"#
        ));
        assert!(
            !text.contains("NEVER"),
            "no rule for an empty list:\n{text}"
        );
        assert!(text.contains(r#"(remote tcp "*:443")"#));
        assert!(!text.contains("SecurityServer"));
        assert!(!text.contains("(deny network*)"));
        let kept: Vec<&str> = params
            .iter()
            .filter(|(k, _)| k.starts_with("KEPT"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert!(kept.contains(&"/home/a/.claude/settings.json"), "{kept:?}");
        assert!(kept.contains(&"/work/p/.git"), "{kept:?}");
    }
}
