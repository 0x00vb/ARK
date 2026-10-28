//! The system `ssh`, and one shared connection per setup.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::OnceCell;

use crate::config::{parse_resolved, KnownHost};
use crate::target::{command_line, Target};
use crate::{Failure, Kind, Step};

/// The longest Unix socket path ARK uses, in bytes. macOS allows 103 plus the final NUL,
/// Linux 107; the smaller one holds on both ends.
pub const SOCKET_MAX: usize = 103;

/// `ssh` makes a master socket at `<ControlPath>.<16 random characters>` and then renames it,
/// so the path itself has to leave room for those 17 bytes.
const CONTROL_MAX: usize = SOCKET_MAX - 17;

/// The `ssh` program, and the config file it reads when not the person's own.
#[derive(Clone, Debug)]
pub struct Ssh {
    program: PathBuf,
    config: Option<PathBuf>,
    /// ARK's key: [`ark_key`] unless [`Ssh::key`] says otherwise.
    key: Option<PathBuf>,
}

impl Ssh {
    /// The system's `ssh`, or None when it is not installed. `config` is passed as `-F`;
    /// None reads the person's `~/.ssh/config` and the system's, as `ssh` does.
    pub fn system(config: Option<PathBuf>) -> Option<Ssh> {
        Some(Ssh {
            program: ark_os::find::program("ssh")?,
            config,
            key: ark_key(),
        })
    }

    pub fn new(program: PathBuf, config: Option<PathBuf>) -> Ssh {
        Ssh {
            program,
            config,
            key: ark_key(),
        }
    }

    /// ARK's key at `path` instead of `~/.ssh/ark_ed25519`.
    pub fn key(mut self, path: PathBuf) -> Ssh {
        self.key = Some(path);
        self
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn config(&self) -> Option<&Path> {
        self.config.as_deref()
    }

    /// `ssh` with `-F <config>` when one is set, and ARK's key ([`ark_key`]) once it exists.
    pub(crate) fn command(&self) -> tokio::process::Command {
        let mut c = ark_os::process::command(&self.program);
        if let Some(f) = &self.config {
            c.arg("-F").arg(f);
        }
        if let Some(k) = self.key.as_ref().filter(|k| k.is_file()) {
            c.arg("-i").arg(k);
        }
        c.stdin(Stdio::null()).kill_on_drop(true);
        c
    }

    /// argv for a terminal: the program, `-F <config>`, then `rest`.
    pub(crate) fn argv(&self, rest: &[String]) -> Vec<String> {
        let mut v = vec![self.program.to_string_lossy().into_owned()];
        if let Some(f) = &self.config {
            v.push("-F".into());
            v.push(f.to_string_lossy().into_owned());
        }
        v.extend(rest.iter().cloned());
        v
    }

    /// What `target` resolves to (`ssh -G`), applying `Match` blocks and defaults.
    pub async fn resolve(&self, target: &Target) -> Option<KnownHost> {
        let mut c = self.command();
        c.arg("-G").arg("--").arg(target.to_string());
        c.stdout(Stdio::piped()).stderr(Stdio::null());
        let out = tokio::time::timeout(Duration::from_secs(10), c.output())
            .await
            .ok()?
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let mut host = parse_resolved(target.host(), &String::from_utf8_lossy(&out.stdout))?;
        host.name = target.to_string();
        Some(host)
    }
}

/// A short, stable name for files about `parts`: 16 hex characters of their SHA-256.
pub(crate) fn short_hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    h.finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Make `dir` (and its parents) and let only its owner in (0700).
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    // Made 0700, or refused when another user owns it, others can write to it, or it is a
    // link: another user of this computer must not be able to swap the sockets in it.
    ark_os::fs::private_dir(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod private_tests {
    use super::*;

    #[test]
    fn a_folder_others_can_change_or_a_link_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let open = tmp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(private_dir(&open).is_err());
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(tmp.path(), &link).unwrap();
        assert!(private_dir(&link).is_err());
        let ok = tmp.path().join("ok");
        std::fs::create_dir(&ok).unwrap();
        std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&ok).unwrap();
        let mode = std::fs::metadata(&ok).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "tightened");
    }
}

/// What a remote command gets on its standard input.
pub(crate) enum Input<'a> {
    None,
    Bytes(&'a [u8]),
    File(&'a Path),
}

/// A finished remote command.
pub(crate) struct Ran {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// `ssh`'s or the command's last useful stderr line.
    pub fn last_line(&self) -> String {
        last_line(&self.stderr)
    }
}

/// The last stderr line that says something, without `ssh`'s notes about known hosts.
pub(crate) fn last_line(stderr: &str) -> String {
    stderr
        .lines()
        .map(|l| l.trim_end_matches('\r').trim())
        .rfind(|l| !l.is_empty() && !l.starts_with("Warning: Permanently added"))
        .unwrap_or("")
        .to_string()
}

/// One setup's connection to a computer. Every command shares one master connection
/// (`ControlMaster=auto`, `ControlPersist=60`), so the person's agent or key is asked once.
pub struct Session {
    ssh: Ssh,
    target: Target,
    control: PathBuf,
    home: OnceCell<String>,
}

impl Session {
    /// A session for `target`. Its master socket lives in `<state_dir>/c/` (made 0700). Fails
    /// when that path is too long for a Unix socket. Nothing connects yet.
    pub fn new(ssh: Ssh, target: Target, state_dir: &Path) -> std::io::Result<Session> {
        let dir = state_dir.join("c");
        private_dir(&dir)?;
        let config = ssh
            .config()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let control = dir.join(short_hash(&[&target.to_string(), &config]));
        let len = control.as_os_str().len();
        if len > CONTROL_MAX {
            return Err(std::io::Error::other(format!(
                "The folder for connections is too long for a socket ({len} bytes, at most {CONTROL_MAX}): {}",
                control.display()
            )));
        }
        Ok(Session {
            ssh,
            target,
            control,
            home: OnceCell::new(),
        })
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    pub fn ssh(&self) -> &Ssh {
        &self.ssh
    }

    /// The name in the window's words: the host as the person typed it.
    pub fn name(&self) -> &str {
        self.target.host()
    }

    fn control_options(&self) -> Vec<OsString> {
        let mut path = OsString::from("ControlPath=");
        path.push(self.control.as_os_str());
        vec![
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            path,
            "-o".into(),
            "ControlPersist=60".into(),
        ]
    }

    /// Run `remote` (a command line already quoted) on the computer. ssh's own failures
    /// (exit 255) come back as a [`Failure`] for `step`; the command's exit code is the
    /// caller's to read.
    pub(crate) async fn run(
        &self,
        step: Step,
        remote: &str,
        input: Input<'_>,
        limit: Duration,
    ) -> Result<Ran, Failure> {
        let mut c = self.ssh.command();
        c.arg("-T")
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"])
            .args(["-o", "RemoteCommand=none", "-o", "ClearAllForwardings=yes"])
            .args(["-o", "ServerAliveInterval=15"])
            .args(self.control_options())
            .arg("--")
            .arg(self.target.to_string())
            .arg(remote);
        if !matches!(input, Input::None) {
            c.stdin(Stdio::piped());
        }
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c
            .spawn()
            .map_err(|e| Failure::new(step, Kind::Failed, format!("Could not start ssh: {e}")))?;
        let out = Collect::start(child.stdout.take());
        let err = Collect::start(child.stderr.take());
        let feed = child.stdin.take().map(|mut stdin| {
            let bytes = match input {
                Input::Bytes(b) => Some(b.to_vec()),
                _ => None,
            };
            let file = match input {
                Input::File(p) => Some(p.to_path_buf()),
                _ => None,
            };
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                if let Some(b) = bytes {
                    let _ = stdin.write_all(&b).await;
                } else if let Some(p) = file {
                    if let Ok(mut f) = tokio::fs::File::open(&p).await {
                        let _ = tokio::io::copy(&mut f, &mut stdin).await;
                    }
                }
                let _ = stdin.shutdown().await;
            })
        });
        let status = match tokio::time::timeout(limit, child.wait()).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                return Err(Failure::new(step, Kind::Failed, format!("ssh failed: {e}")));
            }
            Err(_) => {
                let _ = child.kill().await;
                return Err(Failure::new(
                    step,
                    Kind::Unreachable,
                    format!(
                        "{} did not answer within {} s",
                        self.name(),
                        limit.as_secs()
                    ),
                ));
            }
        };
        if let Some(f) = feed {
            f.abort();
        }
        let ran = Ran {
            code: status.code(),
            stdout: out.finish().await,
            stderr: err.finish().await,
        };
        if ran.code == Some(255) {
            return Err(self.ssh_failure(step, &ran.stderr));
        }
        Ok(ran)
    }

    /// [`run`](Self::run) with the words quoted into one command line.
    pub(crate) async fn run_words<S: AsRef<str>>(
        &self,
        step: Step,
        words: &[S],
        input: Input<'_>,
        limit: Duration,
    ) -> Result<Ran, Failure> {
        self.run(step, &command_line(words), input, limit).await
    }

    /// `sh -c <script> sh <args...>`: the script gets its values as `$1`, `$2`..., never
    /// pasted into its text.
    pub(crate) async fn script<S: AsRef<str>>(
        &self,
        step: Step,
        script: &str,
        args: &[S],
        input: Input<'_>,
        limit: Duration,
    ) -> Result<Ran, Failure> {
        let mut words: Vec<&str> = vec!["sh", "-c", script, "sh"];
        words.extend(args.iter().map(|a| a.as_ref()));
        self.run_words(step, &words, input, limit).await
    }

    /// The home folder there, asked once per session.
    pub(crate) async fn home(&self, step: Step) -> Result<&str, Failure> {
        self.home
            .get_or_try_init(|| async {
                let ran = self
                    .run(step, "printf '%s\\n' \"$HOME\"", Input::None, secs(30))
                    .await?;
                let home = ran.stdout.lines().last().unwrap_or("").trim().to_string();
                if !ran.ok() || !home.starts_with('/') {
                    return Err(Failure::new(
                        step,
                        Kind::Failed,
                        format!("{} did not say where its home folder is", self.name()),
                    ));
                }
                Ok(home)
            })
            .await
            .map(String::as_str)
    }

    /// ssh's own failure, sorted into a [`Kind`], in the ADR's words.
    fn ssh_failure(&self, step: Step, stderr: &str) -> Failure {
        let line = last_line(stderr);
        let kind = kind_of(stderr);
        let words = if line.is_empty() {
            format!("Could not reach {}", self.name())
        } else {
            format!("Could not reach {}: {line}", self.name())
        };
        let mut f = Failure::new(step, kind.clone(), words);
        f.terminal = match kind {
            Kind::NeedsTerminal => Some(self.terminal_connect()),
            Kind::AuthRefused => Some(self.terminal_authorize()),
            _ => None,
        };
        f
    }

    /// The command the person runs once in a terminal to answer `ssh` (a new host key, a
    /// passphrase). It opens the same master connection, so the setup can go on after it.
    pub fn terminal_connect(&self) -> Vec<String> {
        let mut rest: Vec<String> = self
            .control_options()
            .into_iter()
            .map(|o| o.to_string_lossy().into_owned())
            .collect();
        rest.extend(["--".into(), self.target.to_string(), "true".into()]);
        self.ssh.argv(&rest)
    }

    /// The command the person runs once in a terminal when the computer refuses every key (it
    /// takes only a password, or the person's key needs a passphrase or a touch): it adds
    /// ARK's key ([`ark_key`]) there, so every later connection, the tunnel's too, needs no
    /// one. `ssh` asks for the password once; that connection stays open as the setup's
    /// master. It ends by logging in with the key alone, and fails when that does not work.
    /// Not on Windows (no `sh`): there the plain connect.
    pub fn terminal_authorize(&self) -> Vec<String> {
        let Some(key) = self.ssh.key.clone().filter(|_| cfg!(unix)) else {
            return self.terminal_connect();
        };
        let mut v: Vec<String> = vec![
            "/bin/sh".into(),
            "-c".into(),
            AUTHORIZE.into(),
            "sh".into(),
            self.target.to_string(),
            key.to_string_lossy().into_owned(),
            self.control.to_string_lossy().into_owned(),
        ];
        v.extend(self.ssh.argv(&[]));
        v
    }

    /// A terminal command that runs `words` there with a terminal (for `sudo`).
    pub(crate) fn terminal_run(&self, words: &[&str]) -> Vec<String> {
        self.ssh.argv(&[
            "-t".into(),
            "--".into(),
            self.target.to_string(),
            command_line(words),
        ])
    }

    /// Close the master connection now instead of after `ControlPersist`.
    pub async fn close(&self) {
        let mut c = self.ssh.command();
        c.args(self.control_options())
            .args(["-O", "exit", "--"])
            .arg(self.target.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = tokio::time::timeout(secs(5), c.status()).await;
    }
}

/// ARK's own key for reaching the person's computers: `~/.ssh/ark_ed25519`, no passphrase.
/// Made by [`Session::terminal_authorize`] when a computer refuses every key; never replaced.
pub fn ark_key() -> Option<PathBuf> {
    ark_os::dirs::home().map(|h| h.join(".ssh").join("ark_ed25519"))
}

/// `$1` the target, `$2` ARK's key, `$3` the master's socket, the rest `ssh` and `-F`. Prints
/// what it does in the person's terminal. The part there runs under `sh`, whatever the login
/// shell, and ends `authorized_keys` with a newline before adding a line.
const AUTHORIZE: &str = r#"t=$1; k=$2; cp=$3; shift 3
if [ ! -e "$k" ]; then
  echo "ARK makes its own key for your computers: $k (no passphrase, only ARK uses it)."
  mkdir -p "${k%/*}" && chmod 700 "${k%/*}" || exit 1
  ssh-keygen -q -t ed25519 -N '' -C ARK -f "$k" || exit 1
elif [ ! -e "$k.pub" ]; then
  ssh-keygen -y -f "$k" > "$k.pub" || exit 1
fi
echo "Adding ARK's key to $t. ssh asks for the password there once."
r='umask 077; mkdir -p ~/.ssh && f=~/.ssh/authorized_keys && k=$(cat) && { grep -qxF "$k" "$f" 2>/dev/null || { if [ -s "$f" ] && [ -n "$(tail -c 1 "$f")" ]; then echo >> "$f"; fi; printf "%s
" "$k" >> "$f"; }; }'
"$@" -T -o RemoteCommand=none -o ControlMaster=auto -o "ControlPath=$cp" -o ControlPersist=60 -- "$t" "sh -c '$r'" < "$k.pub" || exit 1
if "$@" -T -o BatchMode=yes -o ControlMaster=no -o ControlPath=none -o RemoteCommand=none -o IdentitiesOnly=yes -i "$k" -- "$t" true; then
  echo "Done. Go back to ARK and try again."
else
  echo "$t still does not take ARK's key. Its sshd may read keys from elsewhere (AuthorizedKeysFile)."
  exit 1
fi"#;

pub(crate) fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// Which kind of failure ssh's stderr describes.
pub(crate) fn kind_of(stderr: &str) -> Kind {
    let s = stderr.to_ascii_lowercase();
    if s.contains("remote host identification has changed")
        || s.contains("host key for") && s.contains("has changed")
        || s.contains("offending")
    {
        Kind::HostKeyChanged
    } else if s.contains("host key verification failed")
        || s.contains("no ") && s.contains("host key is known")
    {
        Kind::NeedsTerminal
    } else if s.contains("permission denied")
        || s.contains("too many authentication failures")
        || s.contains("no supported authentication methods")
    {
        Kind::AuthRefused
    } else {
        Kind::Unreachable
    }
}

/// Reads a pipe into a buffer in the background. A master connection that `ssh` leaves
/// behind may hold the pipe open, so [`finish`](Collect::finish) waits briefly, not for EOF.
struct Collect {
    buf: Arc<Mutex<Vec<u8>>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Collect {
    fn start<R: AsyncRead + Unpin + Send + 'static>(pipe: Option<R>) -> Collect {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let task = pipe.map(|mut r| {
            let buf = buf.clone();
            tokio::spawn(async move {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = r.read(&mut chunk).await {
                    if n == 0 {
                        break;
                    }
                    buf.lock().unwrap().extend_from_slice(&chunk[..n]);
                }
            })
        });
        Collect { buf, task }
    }

    async fn finish(mut self) -> String {
        if let Some(t) = self.task.take() {
            let abort = t.abort_handle();
            if tokio::time::timeout(secs(2), t).await.is_err() {
                abort.abort();
            }
        }
        let bytes = std::mem::take(&mut *self.buf.lock().unwrap());
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_failures_are_sorted() {
        let changed = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
            @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
            Offending ECDSA key in /x/known_hosts:3\n\
            Host key for [127.0.0.1]:2222 has changed and you have requested strict checking.\n\
            Host key verification failed.\n";
        assert_eq!(kind_of(changed), Kind::HostKeyChanged);
        let unknown = "No ED25519 host key is known for [127.0.0.1]:2222 and you have requested strict checking.\n\
            Host key verification failed.\n";
        assert_eq!(kind_of(unknown), Kind::NeedsTerminal);
        assert_eq!(
            kind_of("reach@127.0.0.1: Permission denied (publickey).\n"),
            Kind::AuthRefused
        );
        assert_eq!(
            kind_of("ssh: connect to host 127.0.0.1 port 9: Connection refused\n"),
            Kind::Unreachable
        );
        assert_eq!(last_line(unknown), "Host key verification failed.");
        assert_eq!(
            last_line("Warning: Permanently added 'x' (ED25519) to the list of known hosts.\r\n"),
            ""
        );
    }

    #[test]
    fn a_long_state_folder_is_refused() {
        let ssh = Ssh::new("ssh".into(), None);
        let t = Target::parse("vps").unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert!(Session::new(ssh.clone(), t.clone(), dir.path()).is_ok());
        let deep = dir.path().join("x".repeat(90));
        let err = Session::new(ssh, t, &deep).err().unwrap();
        assert!(err.to_string().contains("too long"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn the_connection_folder_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let s = Session::new(
            Ssh::new("ssh".into(), None),
            Target::parse("vps").unwrap(),
            dir.path(),
        )
        .unwrap();
        let mode = std::fs::metadata(dir.path().join("c"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(s.control.as_os_str().len() <= CONTROL_MAX);
    }

    /// The terminal step for a computer that refuses every key, with a stand-in `ssh` (no ssh
    /// server is involved): it runs the command meant for there in a home folder of its own,
    /// and a login with the key alone works when the key is in that folder's authorized_keys.
    #[cfg(unix)]
    #[test]
    fn authorize_adds_arks_key_once_and_checks_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (home, there) = (dir.path().join("home"), dir.path().join("there"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(there.join(".ssh")).unwrap();
        let fake = dir.path().join("ssh");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nthere={}\n\
                 for a; do last=$a; case $a in BatchMode=yes) batch=1 ;; esac; done\n\
                 for a; do [ \"$p\" = -i ] && k=$a; p=$a; done\n\
                 if [ -n \"$batch\" ]; then [ -e $there/refuse ] && exit 255; grep -qxF \"$(cat \"$k.pub\")\" $there/.ssh/authorized_keys; exit; fi\n\
                 HOME=$there sh -c \"$last\"\n",
                there.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let key = home.join(".ssh/ark_ed25519");
        let s = Session::new(
            Ssh::new(fake, None).key(key.clone()),
            Target::parse("me@mac").unwrap(),
            dir.path(),
        )
        .unwrap();
        let run = || {
            let argv = s.terminal_authorize();
            std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .output()
                .unwrap()
        };
        let keys = there.join(".ssh/authorized_keys");
        // A line without its newline stays whole.
        std::fs::write(&keys, "ssh-rsa AAAAold other").unwrap();

        let out = run();
        assert!(out.status.success(), "{out:?}");
        let made = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&keys).unwrap(),
            format!("ssh-rsa AAAAold other\n{made}")
        );
        let mode = std::fs::metadata(&key).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);

        // Again, and with the .pub gone: the key is kept, its .pub made from it, added once.
        let private = std::fs::read(&key).unwrap();
        std::fs::remove_file(key.with_extension("pub")).unwrap();
        assert!(run().status.success());
        assert_eq!(std::fs::read(&key).unwrap(), private);
        assert_eq!(std::fs::read_to_string(&keys).unwrap().lines().count(), 2);

        // A computer that still refuses the key: the step fails and says so.
        std::fs::write(there.join("refuse"), "").unwrap();
        let out = run();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("still does not take"));
    }

    #[test]
    fn terminal_commands_keep_the_target_after_dashes() {
        let s = Session::new(
            Ssh::new("/usr/bin/ssh".into(), Some("/c/config".into())),
            Target::parse("me@vps").unwrap(),
            tempfile::tempdir().unwrap().path(),
        )
        .unwrap();
        let argv = s.terminal_connect();
        assert_eq!(&argv[..3], ["/usr/bin/ssh", "-F", "/c/config"]);
        assert_eq!(&argv[argv.len() - 3..], ["--", "me@vps", "true"]);
        let s = Session::new(
            Ssh::new("/usr/bin/ssh".into(), Some("/c/config".into())).key("/k/ark".into()),
            Target::parse("me@vps").unwrap(),
            tempfile::tempdir().unwrap().path(),
        )
        .unwrap();
        let argv = s.terminal_authorize();
        assert_eq!(&argv[..2], ["/bin/sh", "-c"]);
        assert_eq!(&argv[3..6], ["sh", "me@vps", "/k/ark"]);
        assert_eq!(&argv[argv.len() - 3..], ["/usr/bin/ssh", "-F", "/c/config"]);
        let argv = s.terminal_run(&["sudo", "loginctl", "enable-linger", "me"]);
        assert_eq!(
            &argv[argv.len() - 3..],
            ["--", "me@vps", "'sudo' 'loginctl' 'enable-linger' 'me'"]
        );
    }
}
