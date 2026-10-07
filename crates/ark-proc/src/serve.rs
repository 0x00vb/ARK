//! Whether a process this service started is the one listening on a port (ARK ADR 0046).
//!
//! A process is Ready when its address answers, whoever answers. That is enough to show the
//! person a page, but not to let an agent's browser read the address: a command that prints
//! `http://localhost:8080` (or names `port: 8080`) and serves nothing would otherwise lend the
//! agent another app's page. So the listening socket must belong to the process's own group
//! (each process starts in a group of its own, its id).

use std::collections::HashSet;

/// The TCP ports on this computer listened on by processes of the groups in `groups`, with
/// their group: one question to the system for every port. Empty when it cannot be told (no
/// `lsof` on macOS, no `/proc` on Linux, another system).
pub(crate) fn listeners(groups: &HashSet<u32>) -> HashSet<(u16, u32)> {
    if groups.is_empty() {
        return HashSet::new();
    }
    imp::listeners(groups)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::HashSet;

    /// What `lsof` lists, or `None` if it does not answer in two seconds (a stale network
    /// mount can hang it): the agent's turn waits on this.
    fn lsof() -> Option<String> {
        use std::io::Read;
        use std::process::{Command, Stdio};
        let mut child = Command::new("/usr/sbin/lsof")
            .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut out = child.stdout.take()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = out.read_to_string(&mut text);
            let _ = tx.send(text);
        });
        let text = rx.recv_timeout(std::time::Duration::from_secs(2)).ok();
        if text.is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
        text
    }

    pub fn listeners(groups: &HashSet<u32>) -> HashSet<(u16, u32)> {
        let Some(out) = lsof() else {
            return HashSet::new();
        };
        let mut found = HashSet::new();
        let mut group = None;
        for line in out.lines() {
            if let Some(pid) = line.strip_prefix('p') {
                // SAFETY: getpgid only reads the group of a process id.
                group = pid
                    .parse::<i32>()
                    .ok()
                    .map(|pid| unsafe { libc::getpgid(pid) })
                    .and_then(|g| u32::try_from(g).ok())
                    .filter(|g| groups.contains(g));
            } else if let (Some(g), Some(name)) = (group, line.strip_prefix('n')) {
                // `127.0.0.1:3000`, `*:3000`, `[::1]:3000`
                if let Some(port) = name.rsplit(':').next().and_then(|p| p.parse().ok()) {
                    found.insert((port, g));
                }
            }
        }
        found
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::{HashMap, HashSet};

    /// Sockets listening, by inode (`socket:[123]`), with their port.
    fn listening() -> HashMap<String, u16> {
        let mut found = HashMap::new();
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            let Ok(text) = std::fs::read_to_string(table) else {
                continue;
            };
            for row in text.lines().skip(1) {
                let cols: Vec<&str> = row.split_whitespace().collect();
                let (Some(local), Some(state), Some(inode)) =
                    (cols.get(1), cols.get(3), cols.get(9))
                else {
                    continue;
                };
                let port = local
                    .rsplit(':')
                    .next()
                    .and_then(|p| u16::from_str_radix(p, 16).ok());
                // 0A: LISTEN.
                if let (Some(port), "0A") = (port, *state) {
                    found.insert(format!("socket:[{inode}]"), port);
                }
            }
        }
        found
    }

    /// The process group of `pid`, from `/proc/<pid>/stat`: the third field after the
    /// command, which is in parentheses and may hold spaces and `)`.
    fn group(pid: &str) -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after = &stat[stat.rfind(')')? + 1..];
        after.split_whitespace().nth(2)?.parse().ok()
    }

    pub fn listeners(groups: &HashSet<u32>) -> HashSet<(u16, u32)> {
        let sockets = listening();
        let mut found = HashSet::new();
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return found;
        };
        for e in procs.flatten() {
            let pid = e.file_name().to_string_lossy().into_owned();
            if !pid.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Some(g) = group(&pid).filter(|g| groups.contains(g)) else {
                continue;
            };
            let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                if let Ok(link) = std::fs::read_link(fd.path()) {
                    if let Some(port) = sockets.get(link.to_string_lossy().as_ref()) {
                        found.insert((*port, g));
                    }
                }
            }
        }
        found
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use std::collections::HashSet;

    pub fn listeners(_groups: &HashSet<u32>) -> HashSet<(u16, u32)> {
        HashSet::new()
    }
}
