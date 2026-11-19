//! A local Unix socket forwarded to the computer's (`ssh -N -L local:remote`), kept up.
//!
//! Unix sockets on both ends, not a TCP port: another user on either computer cannot reach
//! a socket in a 0700 folder. `ssh` gives the local one mode 0600 (`StreamLocalBindMask`).
//!
//! `-L` forwards are opened per connection, so a refusal (an `sshd` that turns socket
//! forwarding off, or a service that is not there) shows only when something connects. Each
//! time `ssh` starts, the tunnel connects once to find out before it says [`TunnelState::Up`].
//! When that connection is refused, a plain `ssh` command asks whether the socket answers on
//! the computer: if it does, `sshd` is what refuses.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::watch;

use crate::ssh::{last_line, private_dir, short_hash, Ssh, SOCKET_MAX};
use crate::target::{command_line, Target};

/// Where the tunnel is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum TunnelState {
    /// The first `ssh` is starting.
    Connecting,
    /// Connections to [`Tunnel::socket`] reach the computer's socket.
    Up,
    /// Not answering, in words the window can show. It tries again with backoff.
    Down(String),
}

/// A tunnel kept up in the background. Dropping it stops `ssh` and removes the socket.
pub struct Tunnel {
    socket: PathBuf,
    state: watch::Receiver<TunnelState>,
    task: tokio::task::JoinHandle<()>,
}

/// First wait after `ssh` ends; it doubles up to [`MAX_WAIT`] and goes back after an `Up`.
const FIRST_WAIT: Duration = Duration::from_secs(1);
const MAX_WAIT: Duration = Duration::from_secs(60);

impl Tunnel {
    /// Forward a socket in `local_dir` (made 0700) to `remote_socket` on `target`. Fails when
    /// a path is too long for a Unix socket or has a `:` in it. Returns at once; watch
    /// [`state`](Self::state) or call [`wait_up`](Self::wait_up).
    pub fn open(
        ssh: &Ssh,
        target: &Target,
        local_dir: &Path,
        remote_socket: &str,
    ) -> std::io::Result<Tunnel> {
        if cfg!(not(unix)) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Linking a computer over SSH needs Unix sockets, which this system does not offer",
            ));
        }
        private_dir(local_dir)?;
        let config = ssh
            .config()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let socket = local_dir.join(format!(
            "{}.sock",
            short_hash(&[&target.to_string(), &config, remote_socket])
        ));
        let local = socket.to_string_lossy().into_owned();
        for p in [local.as_str(), remote_socket] {
            if p.len() > SOCKET_MAX || p.contains(':') || !p.starts_with('/') {
                return Err(std::io::Error::other(format!(
                    "This path cannot be a socket for the tunnel: {p}"
                )));
            }
        }
        let (tx, state) = watch::channel(TunnelState::Connecting);
        let job = Job {
            ssh: ssh.clone(),
            target: target.clone(),
            local: socket.clone(),
            remote: remote_socket.to_string(),
        };
        let task = tokio::spawn(job.keep(tx));
        Ok(Tunnel {
            socket,
            state,
            task,
        })
    }

    /// The local socket: connect here to reach the computer's.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The state now and every change.
    pub fn state(&self) -> watch::Receiver<TunnelState> {
        self.state.clone()
    }

    /// Wait until the tunnel is up. Err with the reason when it goes down first, or after
    /// `limit`.
    pub async fn wait_up(&self, limit: Duration) -> Result<(), String> {
        let mut rx = self.state.clone();
        let done = tokio::time::timeout(
            limit,
            rx.wait_for(|s| matches!(s, TunnelState::Up | TunnelState::Down(_))),
        )
        .await;
        match done {
            Ok(Ok(s)) => match &*s {
                TunnelState::Up => Ok(()),
                TunnelState::Down(why) => Err(why.clone()),
                TunnelState::Connecting => unreachable!(),
            },
            Ok(Err(_)) => Err("The tunnel stopped".into()),
            Err(_) => Err(format!(
                "The tunnel did not come up within {} s",
                limit.as_secs()
            )),
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket);
    }
}

struct Job {
    ssh: Ssh,
    target: Target,
    local: PathBuf,
    remote: String,
}

/// How one `ssh` run ended.
enum Ended {
    /// It was up, then `ssh` ended.
    AfterUp(String),
    /// It never came up.
    Before(String),
}

impl Job {
    async fn keep(self, tx: watch::Sender<TunnelState>) {
        let mut wait = FIRST_WAIT;
        loop {
            match self.once(&tx).await {
                Ended::AfterUp(why) => {
                    wait = FIRST_WAIT;
                    tx.send_replace(TunnelState::Down(why));
                }
                Ended::Before(why) => {
                    tx.send_replace(TunnelState::Down(why));
                }
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(MAX_WAIT);
        }
    }

    fn name(&self) -> &str {
        self.target.host()
    }

    fn refused_words(&self) -> String {
        format!("{} does not allow forwarding (sshd_config)", self.name())
    }

    /// Words for a tunnel that is not answering, from `ssh`'s last stderr lines.
    fn reason(&self, lines: &[String]) -> String {
        let all = lines.join("\n").to_ascii_lowercase();
        if all.contains("administratively prohibited") {
            return self.refused_words();
        }
        let last = last_line(&lines.join("\n"));
        if last.is_empty() {
            format!("{} is not answering", self.name())
        } else {
            format!("{} is not answering: {last}", self.name())
        }
    }

    /// Whether the socket answers on the computer itself, asked over a plain `ssh` command.
    /// `sshd` answers a refused socket forward as "connect failed", the same words as for a
    /// socket that is not there, so this tells the two apart.
    async fn answers_there(&self) -> bool {
        let script = "s=$1; [ -S \"$s\" ] || exit 1; command -v curl >/dev/null 2>&1 || exit 0; curl -s -o /dev/null --max-time 3 --unix-socket \"$s\" http://localhost/";
        let mut c = self.ssh.command();
        c.args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15"])
            .args(["-o", "ControlMaster=no", "-o", "ControlPath=none"])
            .args(["-o", "RemoteCommand=none", "-o", "ClearAllForwardings=yes"])
            .arg("--")
            .arg(self.target.to_string())
            .arg(command_line(&["sh", "-c", script, "sh", &self.remote]))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        matches!(
            tokio::time::timeout(Duration::from_secs(20), c.status()).await,
            Ok(Ok(s)) if s.success()
        )
    }

    async fn once(&self, tx: &watch::Sender<TunnelState>) -> Ended {
        let _ = std::fs::remove_file(&self.local);
        let mut forward = self.local.to_string_lossy().into_owned();
        forward.push(':');
        forward.push_str(&self.remote);
        let mut c = self.ssh.command();
        c.args(["-N", "-T"])
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"])
            .args(["-o", "ControlMaster=no", "-o", "ControlPath=none"])
            .args(["-o", "ExitOnForwardFailure=yes"])
            .args(["-o", "StreamLocalBindUnlink=yes"])
            .args([
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
            ])
            .args(["-o", "RemoteCommand=none"])
            .arg("-L")
            .arg(&forward)
            .arg("--")
            .arg(self.target.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = match c.spawn() {
            Ok(c) => c,
            Err(e) => return Ended::Before(format!("Could not start ssh: {e}")),
        };
        let mut tree = ark_os::process::Tree::of(child.id());
        let lines = Arc::new(Mutex::new(VecDeque::<String>::new()));
        if let Some(err) = child.stderr.take() {
            let lines = lines.clone();
            tokio::spawn(async move {
                let mut r = BufReader::new(err).lines();
                while let Ok(Some(l)) = r.next_line().await {
                    let mut v = lines.lock().unwrap();
                    v.push_back(l);
                    if v.len() > 20 {
                        v.pop_front();
                    }
                }
            });
        }
        let seen = |lines: &Arc<Mutex<VecDeque<String>>>| -> Vec<String> {
            lines.lock().unwrap().iter().cloned().collect()
        };

        // Wait for the local socket to take connections, or for ssh to end.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
        let probe = loop {
            if let Ok(Some(_)) = child.try_wait() {
                tree.disarm();
                tokio::time::sleep(Duration::from_millis(100)).await;
                return Ended::Before(self.reason(&seen(&lines)));
            }
            if self.local.exists() {
                if let Some(s) = probe(&self.local).await {
                    break s;
                }
            }
            if tokio::time::Instant::now() > deadline {
                return Ended::Before(format!("{} did not answer", self.name()));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        // ssh closes the connection at once when the far end refuses it. A server that
        // took it waits for a request instead.
        if refused(probe).await {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(child);
            let mut why = self.reason(&seen(&lines));
            if !why.contains("forwarding") && self.answers_there().await {
                why = self.refused_words();
            }
            return Ended::Before(why);
        }
        tx.send_replace(TunnelState::Up);
        let _ = child.wait().await;
        tree.disarm();
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ended::AfterUp(self.reason(&seen(&lines)))
    }
}

/// One connection to the local end of the tunnel.
#[cfg(unix)]
async fn probe(local: &Path) -> Option<tokio::net::UnixStream> {
    tokio::net::UnixStream::connect(local).await.ok()
}

/// Tokio has no Unix sockets off Unix, and [`Tunnel::open`] refuses there before a job starts.
#[cfg(not(unix))]
async fn probe(_local: &Path) -> Option<tokio::io::Empty> {
    None
}

/// True when the connection is closed from the far side within a second.
async fn refused(mut s: impl tokio::io::AsyncRead + Unpin) -> bool {
    let mut b = [0u8; 1];
    matches!(
        tokio::time::timeout(Duration::from_secs(1), s.read(&mut b)).await,
        Ok(Ok(0)) | Ok(Err(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn paths_that_cannot_be_sockets_are_refused() {
        let ssh = Ssh::new("ssh".into(), None);
        let t = Target::parse("vps").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let long = format!("/{}", "x".repeat(SOCKET_MAX));
        for remote in [long.as_str(), "/a:b", "relative/s"] {
            assert!(
                Tunnel::open(&ssh, &t, dir.path(), remote).is_err(),
                "{remote}"
            );
        }
        let deep = dir.path().join("y".repeat(100));
        assert!(Tunnel::open(&ssh, &t, &deep, "/h/s").is_err());
    }

    #[test]
    fn reasons_use_the_window_words() {
        let job = Job {
            ssh: Ssh::new("ssh".into(), None),
            target: Target::parse("vps").unwrap(),
            local: "/x".into(),
            remote: "/y".into(),
        };
        assert_eq!(
            job.reason(&[
                "channel 2: open failed: administratively prohibited: open failed".into()
            ]),
            "vps does not allow forwarding (sshd_config)"
        );
        assert_eq!(
            job.reason(&["Connection closed by 127.0.0.1 port 2222".into()]),
            "vps is not answering: Connection closed by 127.0.0.1 port 2222"
        );
        assert_eq!(job.reason(&[]), "vps is not answering");
    }
}
