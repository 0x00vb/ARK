//! The computers named in the person's SSH config.
//!
//! ARK reads only `Host` names (following `Include`), then asks `ssh -G` what each resolves
//! to, so `Match` blocks, `CanonicalizeHostname` and system defaults apply as they do for
//! `ssh` itself. Patterns (`*`, `?`, `!`) are not computers and are skipped.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::ssh::Ssh;
use crate::target::Target;

/// A computer from the SSH config, as `ssh -G` resolves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KnownHost {
    /// The `Host` name: what the person types and what ARK passes to `ssh`.
    pub name: String,
    pub user: String,
    pub hostname: String,
    pub port: u16,
}

/// OpenSSH stops at this depth too.
const MAX_DEPTH: usize = 16;

/// `Host` names in `config` and the files it includes, in order, without patterns or
/// duplicates. A relative `Include` is relative to `ssh_dir` (`~/.ssh` for a user config).
pub fn host_names(config: &Path, ssh_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    read(config, ssh_dir, 0, &mut names);
    names
}

fn read(file: &Path, ssh_dir: &Path, depth: usize, names: &mut Vec<String>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    for line in text.lines() {
        let Some((key, args)) = split_line(line) else {
            continue;
        };
        if key.eq_ignore_ascii_case("host") {
            for a in args {
                let plain = !a.contains(['*', '?', '!']);
                if plain && Target::parse(&a).is_ok() && !names.contains(&a) {
                    names.push(a);
                }
            }
        } else if key.eq_ignore_ascii_case("include") {
            for a in args {
                for f in glob(&include_path(&a, ssh_dir)) {
                    read(&f, ssh_dir, depth + 1, names);
                }
            }
        }
    }
}

/// `Keyword args...` or `Keyword=args...`, with `"quoted args"`. None for blanks and comments.
fn split_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let key = line[..end].to_string();
    let mut rest = line[end..].trim_start();
    if let Some(r) = rest.strip_prefix('=') {
        rest = r.trim_start();
    }
    let mut args = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in rest.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '#' if !quoted && !started => break,
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(word);
    }
    Some((key, args))
}

fn include_path(arg: &str, ssh_dir: &Path) -> PathBuf {
    if let Some(rest) = arg.strip_prefix("~/") {
        if let Some(home) = ark_os::dirs::home() {
            return home.join(rest);
        }
    }
    let p = Path::new(arg);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        ssh_dir.join(p)
    }
}

/// Files matching `pattern`, where any component may hold `*` or `?`. Sorted, as glob(3).
fn glob(pattern: &Path) -> Vec<PathBuf> {
    let mut found = vec![PathBuf::new()];
    for part in pattern.components() {
        let s = part.as_os_str().to_string_lossy();
        if !s.contains(['*', '?']) {
            for f in &mut found {
                f.push(part);
            }
            continue;
        }
        let mut next = Vec::new();
        for dir in &found {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.') && wild(&s, n))
                .collect();
            names.sort();
            next.extend(names.into_iter().map(|n| dir.join(n)));
        }
        found = next;
    }
    found.retain(|f| f.is_file());
    found
}

/// `*` and `?` matching, as in a file name pattern.
fn wild(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut i, mut j) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        if i < p.len() && (p[i] == '?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some((i, j));
            i += 1;
        } else if let Some((si, sj)) = star {
            i = si + 1;
            j = sj + 1;
            star = Some((si, sj + 1));
        } else {
            return false;
        }
    }
    p[i..].iter().all(|c| *c == '*')
}

/// `user`, `hostname` and `port` from `ssh -G` output.
pub(crate) fn parse_resolved(name: &str, text: &str) -> Option<KnownHost> {
    let mut user = None;
    let mut hostname = None;
    let mut port = None;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(' ') else {
            continue;
        };
        match k {
            "user" => user = Some(v.trim().to_string()),
            "hostname" => hostname = Some(v.trim().to_string()),
            "port" => port = v.trim().parse().ok(),
            _ => {}
        }
    }
    Some(KnownHost {
        name: name.to_string(),
        user: user?,
        hostname: hostname?,
        port: port?,
    })
}

/// The computers in the person's SSH config (`ssh_config`, else `~/.ssh/config`), each
/// resolved by `ssh -G`. Empty when there is no `ssh` or no config.
pub async fn hosts(ssh_config: Option<&Path>) -> Vec<KnownHost> {
    match Ssh::system(ssh_config.map(Path::to_path_buf)) {
        Some(ssh) => ssh.hosts().await,
        None => Vec::new(),
    }
}

impl Ssh {
    /// [`hosts`] with this `ssh` and its config.
    pub async fn hosts(&self) -> Vec<KnownHost> {
        let Some(home) = ark_os::dirs::home() else {
            return Vec::new();
        };
        let ssh_dir = home.join(".ssh");
        let config = self
            .config()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| ssh_dir.join("config"));
        let mut out = Vec::new();
        for name in host_names(&config, &ssh_dir) {
            let Ok(target) = Target::parse(&name) else {
                continue;
            };
            if let Some(h) = self.resolve(&target).await {
                out.push(h);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_like_ssh() {
        assert_eq!(
            split_line("  Host a b  # c"),
            Some(("Host".into(), vec!["a".into(), "b".into()]))
        );
        assert_eq!(
            split_line("Include=\"my dir/*.conf\""),
            Some(("Include".into(), vec!["my dir/*.conf".into()]))
        );
        assert_eq!(split_line("# only a comment"), None);
        assert_eq!(split_line("   "), None);
    }

    #[test]
    fn wildcards_match_names() {
        assert!(wild("*.conf", "a.conf"));
        assert!(!wild("*.conf", "a.conf.bak"));
        assert!(wild("h?st", "host"));
        assert!(wild("*", "x"));
        assert!(!wild("a*b", "ac"));
    }

    #[test]
    fn hosts_follow_include_and_skip_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir(d.join("conf.d")).unwrap();
        std::fs::write(
            d.join("config"),
            "Include conf.d/*.conf\n\
             Host vps build-01 *.internal !bad\n  User me\n\
             Host vps\n\
             Match host x exec \"true\"\n\
             Host \"quoted\" ?ingle -oProxyCommand=x a'b\n\
             Include missing/*.conf /nowhere/at/all\n",
        )
        .unwrap();
        std::fs::write(d.join("conf.d/b.conf"), "Host second\n").unwrap();
        std::fs::write(d.join("conf.d/a.conf"), "Host first\nInclude nested.conf\n").unwrap();
        std::fs::write(d.join("conf.d/c.txt"), "Host not-included\n").unwrap();
        std::fs::write(d.join("nested.conf"), "Host deep\n").unwrap();
        let names = host_names(&d.join("config"), d);
        assert_eq!(
            names,
            ["first", "deep", "second", "vps", "build-01", "quoted"]
        );
    }

    #[test]
    fn an_include_loop_ends() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::write(d.join("config"), "Host a\nInclude config\n").unwrap();
        assert_eq!(host_names(&d.join("config"), d), ["a"]);
    }

    #[test]
    fn resolved_output_is_read() {
        let text = "user deploy\nhostname 203.0.113.9\nport 2222\nbatchmode no\n";
        assert_eq!(
            parse_resolved("vps", text),
            Some(KnownHost {
                name: "vps".into(),
                user: "deploy".into(),
                hostname: "203.0.113.9".into(),
                port: 2222
            })
        );
        assert_eq!(parse_resolved("vps", "user a\n"), None);
    }
}
