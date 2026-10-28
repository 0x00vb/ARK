//! The steps that set up a computer: connect, look, install, start, pair. And remove.
//!
//! On the computer, for a program named `<name>` by the app's [`ServeContract`]:
//!
//! | Path | Holds |
//! |---|---|
//! | `~/.local/share/<name>/<name>` | the program (0755, in a 0700 folder), with `<name>.previous` |
//! | `~/.local/state/<name>/sock/serve.sock` | the socket it serves on, in a 0700 folder |
//! | `~/.local/state/<name>/serve.log`, `serve.pid` | without `systemd --user`: its output and process |
//! | `~/.config/systemd/user/<name>.service` | with `systemd --user`: the service |
//!
//! `$XDG_RUNTIME_DIR` is not used: it goes away when the person's last session ends.
//!
//! Linux and macOS. The scripts try the GNU tool first, then the BSD one: `sha256sum` or
//! `shasum`, `/proc` or `ps`, `stat -c` or `stat -f`. A Mac has no `systemd`, so it serves in
//! the background with a pid file.

use std::path::PathBuf;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::config::KnownHost;
use crate::ssh::{secs, Input, Session, SOCKET_MAX};
use crate::{Failure, Kind, Secret, Step};

/// How the app's program serves, makes tokens and says its version. ARK does not know
/// which program it installs.
pub trait ServeContract: Send + Sync {
    /// The program's file and folder name there: `~/.local/share/<name>/<name>`. Letters,
    /// digits, `.`, `_`, `-`, starting with a letter or digit.
    fn name(&self) -> &str;

    /// Arguments that serve on the Unix socket `socket` and accept the saved tokens. The
    /// program makes the socket mode 0600 and answers HTTP on it.
    fn serve_args(&self, socket: &str) -> Vec<String>;

    /// Arguments that make a token named `token` and print it on standard output.
    fn token_args(&self, token: &str) -> Vec<String>;

    /// Arguments that revoke the token named `token`.
    fn revoke_args(&self, token: &str) -> Vec<String>;

    /// Arguments that print the program's version.
    fn version_args(&self) -> Vec<String>;

    /// Environment for every run of the program there.
    fn env(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// The program's store, relative to the home folder, for [`remove`] to delete when asked.
    fn store(&self) -> Option<String> {
        None
    }

    /// The version in the version command's output. Default: its first line.
    fn version_of(&self, stdout: &str) -> Option<String> {
        stdout
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
    }

    /// The token in the token command's output. Default: its last line.
    fn token_of(&self, stdout: &str) -> Option<String> {
        stdout
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .map(str::to_string)
    }
}

/// The build to install: a local file, the SHA-256 it must have (hex), and its version.
#[derive(Clone, Debug)]
pub struct Build {
    pub path: PathBuf,
    pub sha256: String,
    /// What its version command prints after install; empty: any, as long as it runs there.
    pub version: String,
}

/// What [`setup`] does.
#[derive(Clone, Copy, Debug)]
pub struct Plan<'a> {
    pub build: &'a Build,
    /// Make a token with this name (named after the computer). None when the app already
    /// holds one: a setup again then only checks.
    pub token: Option<&'a str>,
}

/// What the computer is, from the Look step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Facts {
    pub home: String,
    pub user: String,
    /// `uname -sm`, as it says it: `Linux x86_64`.
    pub system: String,
    /// `linux` or `macos`; else the kernel name as it says it.
    pub os: String,
    /// `x86_64` or `aarch64`; else the machine as it says it.
    pub arch: String,
    /// `glibc 2.36`, `musl`, or None when it could not tell.
    pub libc: Option<String>,
    /// Free space in the home folder, in KiB.
    pub free_kib: Option<u64>,
    /// The program is there.
    pub installed: bool,
    /// Its version, as [`ServeContract::version_of`] reads it.
    pub version: Option<String>,
    /// Its SHA-256, when `sha256sum` or `shasum` is there.
    pub sha256: Option<String>,
    /// `systemctl --user` answers.
    pub systemd: bool,
    /// The person's services keep running after they log out. None: unknown.
    pub linger: Option<bool>,
    /// `sshd` lets Unix sockets be forwarded. None: unknown (`sshd -T` needs root on most
    /// systems); the tunnel finds out.
    pub forwarding: Option<bool>,
}

/// The service, after the Start step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Started {
    /// The socket it serves on there.
    pub socket: String,
    /// Run by `systemd --user`; else started in the background with a pid file.
    pub systemd: bool,
    /// It keeps running after the person logs out. False: the window shows "Stops when you
    /// log out of vps".
    pub linger: bool,
    /// A terminal command that turns lingering on with `sudo`, when the person may run it.
    pub linger_terminal: Option<Vec<String>>,
    /// It was started or restarted by this step.
    pub restarted: bool,
}

/// What [`setup`] found and did.
#[derive(Debug)]
pub struct Setup {
    pub host: KnownHost,
    pub facts: Facts,
    /// The build was sent by this setup.
    pub installed: bool,
    pub started: Started,
    /// The new token, when the plan asked for one.
    pub token: Option<Secret>,
}

/// Set up the computer: connect, look, install, start, and pair when asked. `progress` hears
/// each step as it begins. Run again on a set-up computer, it changes nothing and only checks.
pub async fn setup(
    session: &Session,
    contract: &dyn ServeContract,
    plan: Plan<'_>,
    mut progress: impl FnMut(Step) + Send,
) -> Result<Setup, Failure> {
    progress(Step::Connect);
    let host = session.connect().await?;
    progress(Step::Look);
    let facts = session.look(contract).await?;
    progress(Step::Install);
    let installed = session.install(contract, &facts, plan.build).await?;
    progress(Step::Start);
    let started = session.start(contract, &facts, installed).await?;
    let token = match plan.token {
        Some(name) => {
            progress(Step::Pair);
            Some(session.pair(contract, name).await?)
        }
        None => None,
    };
    Ok(Setup {
        host,
        facts,
        installed,
        started,
        token,
    })
}

/// Take the program off the computer: revoke the token named `token`, stop and disable the
/// service, delete the program and its socket folder, and the store only when
/// `delete_store`. A failed revoke stops here, so the person can try again.
pub async fn remove(
    session: &Session,
    contract: &dyn ServeContract,
    token: Option<&str>,
    delete_store: bool,
) -> Result<(), Failure> {
    let step = Step::Remove;
    let name = checked_name(contract, step)?;
    let home = session.home(step).await?.to_string();
    let p = Paths::new(&home, &name);
    if let Some(token) = token {
        let mut words = vec![p.bin.clone()];
        words.extend(program(contract, &p, contract.revoke_args(token)));
        let ran = session
            .script(
                step,
                "[ -x \"$1\" ] || exit 0; shift; exec \"$@\"",
                &words,
                Input::None,
                secs(60),
            )
            .await?;
        if !ran.ok() {
            return Err(Failure::new(
                step,
                Kind::Failed,
                format!(
                    "Could not revoke the token on {}: {}",
                    session.name(),
                    ran.last_line()
                ),
            ));
        }
    }
    let store = match (delete_store, contract.store()) {
        (true, Some(s)) => Some(checked_store(&s, step)?),
        _ => None,
    };
    let script = "u=$1; f=$2; pf=$3; b=$4; s=$5; st=$6; d=$7; store=$8
if command -v systemctl >/dev/null 2>&1 && [ -f \"$f\" ]; then
  systemctl --user disable --now \"$u\" >/dev/null 2>&1
  rm -f \"$f\"
  systemctl --user daemon-reload >/dev/null 2>&1
fi
if [ -f \"$pf\" ]; then
  o=$(cat \"$pf\")
  if [ -n \"$o\" ] && kill -0 \"$o\" 2>/dev/null && { grep -qaF \"$b\" \"/proc/$o/cmdline\" 2>/dev/null || ps -o command= -p \"$o\" 2>/dev/null | grep -qF \"$b\"; }; then kill \"$o\"; fi
fi
rm -rf \"$s\"
rm -f \"$pf\" \"$st/serve.log\" \"$b\" \"$b.previous\"
rmdir \"$st\" \"$d\" 2>/dev/null
if [ -n \"$store\" ]; then rm -rf -- \"$store\"; fi
exit 0";
    let store = store.map(|s| format!("{home}/{s}")).unwrap_or_default();
    let ran = session
        .script(
            step,
            script,
            &[
                &p.unit_name,
                &p.unit,
                &p.pid,
                &p.bin,
                &p.sock_dir,
                &p.state,
                &p.dir,
                &store,
            ],
            Input::None,
            secs(60),
        )
        .await?;
    if !ran.ok() {
        return Err(Failure::new(
            step,
            Kind::Failed,
            format!(
                "Could not remove from {}: {}",
                session.name(),
                ran.last_line()
            ),
        ));
    }
    Ok(())
}

/// Where things go on the computer for `name`.
struct Paths {
    dir: String,
    bin: String,
    state: String,
    sock_dir: String,
    socket: String,
    pid: String,
    log: String,
    unit_dir: String,
    unit: String,
    unit_name: String,
}

impl Paths {
    fn new(home: &str, name: &str) -> Paths {
        let home = home.trim_end_matches('/');
        let dir = format!("{home}/.local/share/{name}");
        let state = format!("{home}/.local/state/{name}");
        let sock_dir = format!("{state}/sock");
        let unit_dir = format!("{home}/.config/systemd/user");
        Paths {
            bin: format!("{dir}/{name}"),
            socket: format!("{sock_dir}/serve.sock"),
            pid: format!("{state}/serve.pid"),
            log: format!("{state}/serve.log"),
            unit: format!("{unit_dir}/{name}.service"),
            unit_name: format!("{name}.service"),
            dir,
            state,
            sock_dir,
            unit_dir,
        }
    }
}

fn valid_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn checked_name(contract: &dyn ServeContract, step: Step) -> Result<String, Failure> {
    let name = contract.name();
    if !valid_name(name) {
        return Err(Failure::new(
            step,
            Kind::Failed,
            format!("`{name}` cannot be a program's name there"),
        ));
    }
    for (k, v) in contract.env() {
        let key_ok = k.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !key_ok || v.contains(['\n', '\r', '\0']) {
            return Err(Failure::new(
                step,
                Kind::Failed,
                format!("`{k}` cannot be set in the program's environment"),
            ));
        }
    }
    Ok(name.to_string())
}

fn checked_store(store: &str, step: Step) -> Result<String, Failure> {
    let s = store.trim_matches('/');
    let ok = !store.starts_with('/')
        && !s.is_empty()
        && s.split('/').all(|c| !c.is_empty() && c != "." && c != "..");
    if !ok {
        return Err(Failure::new(
            step,
            Kind::Failed,
            format!("`{store}` cannot be deleted as the program's store"),
        ));
    }
    Ok(s.to_string())
}

/// `env K=V ... <bin> args...`, or `<bin> args...` without environment.
fn program(contract: &dyn ServeContract, p: &Paths, args: Vec<String>) -> Vec<String> {
    let env = contract.env();
    let mut words = Vec::new();
    if !env.is_empty() {
        words.push("env".to_string());
        words.extend(env.into_iter().map(|(k, v)| format!("{k}={v}")));
    }
    words.push(p.bin.clone());
    words.extend(args);
    words
}

/// One systemd unit word: double quotes, with `\`, `"`, `$` and `%` escaped.
fn unit_quote(word: &str) -> String {
    let mut out = String::from("\"");
    for c in word.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("$$"),
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn unit_text(contract: &dyn ServeContract, p: &Paths) -> String {
    let mut exec = vec![p.bin.clone()];
    exec.extend(contract.serve_args(&p.socket));
    let mut text =
        format!(
        "# Written by the app that set this computer up. Removing the app's computer removes it.\n\
         [Unit]\nDescription={} on a Unix socket\n\n[Service]\nExecStart={}\n",
        contract.name(),
        exec.iter().map(|w| unit_quote(w)).collect::<Vec<_>>().join(" ")
    );
    for (k, v) in contract.env() {
        text.push_str(&format!(
            "Environment={}\n",
            unit_quote(&format!("{k}={v}"))
        ));
    }
    text.push_str("Restart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n");
    text
}

/// `glibc 2.36` from `getconf GNU_LIBC_VERSION` or the first line of `ldd --version`.
fn libc_of(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let low = raw.to_ascii_lowercase();
    if low.contains("musl") {
        return Some("musl".into());
    }
    if !(low.contains("glibc") || low.contains("gnu libc") || low.starts_with("ldd")) {
        return None;
    }
    let v = raw.split_whitespace().last()?;
    v.starts_with(|c: char| c.is_ascii_digit())
        .then(|| format!("glibc {v}"))
}

fn os_of(kernel: &str) -> Option<&'static str> {
    match kernel {
        "Linux" => Some("linux"),
        "Darwin" => Some("macos"),
        _ => None,
    }
}

fn arch_of(machine: &str) -> Option<&'static str> {
    match machine {
        "x86_64" | "amd64" => Some("x86_64"),
        "aarch64" | "arm64" => Some("aarch64"),
        _ => None,
    }
}

/// The Look step's output, read, and whether ARK has a build for the system. `version` comes
/// from the lines after `version-follows`.
fn read_facts(contract: &dyn ServeContract, home: &str, stdout: &str) -> (Facts, bool) {
    let (head, version_out) = match stdout.split_once("\nversion-follows\n") {
        Some((h, v)) => (h, Some(v)),
        None => (
            stdout.strip_suffix("version-follows\n").unwrap_or(stdout),
            None,
        ),
    };
    let mut f = Facts {
        home: home.to_string(),
        user: String::new(),
        system: String::new(),
        os: String::new(),
        arch: String::new(),
        libc: None,
        free_kib: None,
        installed: false,
        version: None,
        sha256: None,
        systemd: false,
        linger: None,
        forwarding: None,
    };
    let mut fwd_seen = false;
    let mut fwd_refused = false;
    for line in head.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k {
            "user" => f.user = v.to_string(),
            "uname" => f.system = v.to_string(),
            "libc" => f.libc = libc_of(v),
            "disk" => f.free_kib = v.parse().ok(),
            "systemd" => f.systemd = v == "yes",
            "Linger" | "linger" => {
                f.linger = match v {
                    "yes" => Some(true),
                    "no" => Some(false),
                    _ => None,
                }
            }
            "installed" => f.installed = v == "yes",
            "sha256" => {
                f.sha256 = v
                    .split_whitespace()
                    .next()
                    .filter(|h| h.len() == 64)
                    .map(str::to_ascii_lowercase)
            }
            "sshd" => {
                fwd_seen = true;
                let mut it = v.split_whitespace();
                match (it.next(), it.next()) {
                    (Some("allowstreamlocalforwarding"), Some("no" | "remote")) => {
                        fwd_refused = true
                    }
                    (Some("disableforwarding"), Some("yes")) => fwd_refused = true,
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if fwd_seen {
        f.forwarding = Some(!fwd_refused);
    }
    if f.installed {
        f.version = version_out.and_then(|v| contract.version_of(v));
    }
    let mut words = f.system.split_whitespace();
    let (kernel, machine) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let (os, arch) = (os_of(kernel), arch_of(machine));
    f.os = os.unwrap_or(kernel).to_string();
    f.arch = arch.unwrap_or(machine).to_string();
    (f, os.is_some() && arch.is_some())
}

async fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

const LOOK: &str = "b=$1; shift
echo \"user=$(id -un)\"
echo \"uname=$(uname -sm)\"
g=$(getconf GNU_LIBC_VERSION 2>/dev/null)
[ -n \"$g\" ] || g=$(ldd --version 2>&1 | head -n 1)
echo \"libc=$g\"
echo \"disk=$(df -Pk \"$HOME\" 2>/dev/null | awk 'NR==2 {print $4}')\"
if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then echo systemd=yes; fi
u=$(id -un)
if [ -e \"/var/lib/systemd/linger/$u\" ]; then echo linger=yes
elif command -v loginctl >/dev/null 2>&1; then loginctl show-user \"$u\" -p Linger 2>/dev/null; fi
for s in /usr/sbin/sshd /usr/local/sbin/sshd; do
  if [ -x \"$s\" ]; then \"$s\" -T 2>/dev/null | grep -Ei '^(allowstreamlocalforwarding|disableforwarding) ' | sed 's/^/sshd=/'; break; fi
done
if [ -x \"$b\" ]; then
  echo installed=yes
  if command -v sha256sum >/dev/null 2>&1; then echo \"sha256=$(sha256sum \"$b\")\"
  elif command -v shasum >/dev/null 2>&1; then echo \"sha256=$(shasum -a 256 \"$b\")\"; fi
  echo version-follows
  \"$@\" 2>/dev/null
fi
exit 0";

const INSTALL: &str = "set -e
d=$1; n=$2; want=$3
mkdir -p \"$d\"
chmod 700 \"$d\"
t=\"$d/.$n.new\"
cat > \"$t\"
got=
if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum \"$t\" | cut -d ' ' -f 1)
elif command -v shasum >/dev/null 2>&1; then got=$(shasum -a 256 \"$t\" | cut -d ' ' -f 1); fi
if [ -n \"$got\" ]; then
  if [ \"$got\" != \"$want\" ]; then rm -f \"$t\"; echo \"the copy that arrived does not match\" >&2; exit 3; fi
fi
chmod 755 \"$t\"
if [ -e \"$d/$n\" ]; then mv -f \"$d/$n\" \"$d/$n.previous\"; fi
mv -f \"$t\" \"$d/$n\"";

const UNIT: &str = "set -e
dir=$1; f=$2; u=$3; r=$4
mkdir -p \"$dir\"
t=\"$f.new\"
cat > \"$t\"
if [ -f \"$f\" ] && cmp -s \"$t\" \"$f\"; then rm -f \"$t\"; else mv -f \"$t\" \"$f\"; r=1; systemctl --user daemon-reload; fi
systemctl --user enable \"$u\" >/dev/null 2>&1 || true
if [ \"$r\" = 1 ]; then systemctl --user restart \"$u\"; echo restarted
elif systemctl --user is-active --quiet \"$u\"; then echo running
else systemctl --user start \"$u\"; echo restarted; fi";

const BACKGROUND: &str = "pf=$1; log=$2; r=$3; b=$4; shift 4
alive() { kill -0 \"$1\" 2>/dev/null && { grep -qaF \"$b\" \"/proc/$1/cmdline\" 2>/dev/null || ps -o command= -p \"$1\" 2>/dev/null | grep -qF \"$b\"; }; }
o=$(cat \"$pf\" 2>/dev/null || true)
if [ -n \"$o\" ] && alive \"$o\"; then
  if [ \"$r\" != 1 ]; then echo running; exit 0; fi
  kill \"$o\"
  i=0; while alive \"$o\" && [ $i -lt 50 ]; do sleep 0.1; i=$((i+1)); done
fi
cd \"$HOME\"
if command -v setsid >/dev/null 2>&1; then
  setsid nohup \"$@\" >> \"$log\" 2>&1 < /dev/null &
else
  nohup \"$@\" >> \"$log\" 2>&1 < /dev/null &
fi
echo $! > \"$pf\"
echo restarted";

const WAIT: &str = "s=$1; i=0
while [ $i -lt 100 ]; do
  if [ -S \"$s\" ]; then
    if ! command -v curl >/dev/null 2>&1; then echo \"up $(stat -c %a \"$s\" 2>/dev/null || stat -f %Lp \"$s\" 2>/dev/null)\"; exit 0; fi
    if curl -s -o /dev/null --max-time 2 --unix-socket \"$s\" http://localhost/; then echo \"up $(stat -c %a \"$s\" 2>/dev/null || stat -f %Lp \"$s\" 2>/dev/null)\"; exit 0; fi
  fi
  sleep 0.2; i=$((i+1))
done
echo down";

impl Session {
    /// Step 1: one command over a new connection (`true`). An unknown host key or a missing
    /// passphrase comes back as [`Kind::NeedsTerminal`] with the command to run once.
    pub async fn connect(&self) -> Result<KnownHost, Failure> {
        let step = Step::Connect;
        let Some(host) = self.ssh().resolve(self.target()).await else {
            return Err(Failure::new(
                step,
                Kind::Failed,
                format!("ssh could not read its settings for {}", self.name()),
            ));
        };
        let ran = self
            .run_words(step, &["true"], Input::None, secs(45))
            .await?;
        if !ran.ok() {
            return Err(Failure::new(
                step,
                Kind::Unreachable,
                format!("Could not reach {}: {}", self.name(), ran.last_line()),
            ));
        }
        Ok(host)
    }

    /// Step 2: what the computer is. Refuses a system ARK has no build for, and an `sshd`
    /// known to refuse socket forwarding.
    pub async fn look(&self, contract: &dyn ServeContract) -> Result<Facts, Failure> {
        let step = Step::Look;
        let name = checked_name(contract, step)?;
        let home = self.home(step).await?.to_string();
        let p = Paths::new(&home, &name);
        let mut args = vec![p.bin.clone()];
        args.extend(program(contract, &p, contract.version_args()));
        let ran = self
            .script(step, LOOK, &args, Input::None, secs(60))
            .await?;
        let (facts, supported) = read_facts(contract, &home, &ran.stdout);
        if facts.system.is_empty() {
            return Err(Failure::new(
                step,
                Kind::Failed,
                format!("Could not look at {}: {}", self.name(), ran.last_line()),
            ));
        }
        if !supported {
            return Err(Failure::new(
                step,
                Kind::Unsupported,
                format!(
                    "{} runs {}; ARK runs on Linux x86_64 and arm64, and macOS arm64",
                    self.name(),
                    facts.system
                ),
            ));
        }
        if facts.forwarding == Some(false) {
            return Err(Failure::new(
                step,
                Kind::ForwardingRefused,
                format!("{} does not allow forwarding (sshd_config)", self.name()),
            ));
        }
        Ok(facts)
    }

    /// Step 3: check the build's SHA-256 here, then send it over the connection and put it
    /// in place, keeping the one before. Nothing is sent when the hash differs. Skipped when
    /// the program there already has this hash (or, without `sha256sum` there, this version).
    /// True when it sent the build.
    pub async fn install(
        &self,
        contract: &dyn ServeContract,
        facts: &Facts,
        build: &Build,
    ) -> Result<bool, Failure> {
        let step = Step::Install;
        let name = checked_name(contract, step)?;
        let got = sha256_file(&build.path).await.map_err(|e| {
            Failure::new(
                step,
                Kind::Failed,
                format!("Could not read the build {}: {e}", build.path.display()),
            )
        })?;
        let want = build.sha256.trim().to_ascii_lowercase();
        if got != want {
            return Err(Failure::new(
                step,
                Kind::BuildMismatch,
                "The build did not match; nothing was installed",
            ));
        }
        let same = match &facts.sha256 {
            Some(h) => *h == want,
            None => facts.installed && facts.version.as_deref() == Some(build.version.as_str()),
        };
        if same {
            return Ok(false);
        }
        let p = Paths::new(&facts.home, &name);
        let ran = self
            .script(
                step,
                INSTALL,
                &[&p.dir, &name, &want],
                Input::File(&build.path),
                secs(600),
            )
            .await?;
        if ran.code == Some(3) {
            return Err(Failure::new(
                step,
                Kind::BuildMismatch,
                format!(
                    "The copy that reached {} did not match; nothing was installed",
                    self.name()
                ),
            ));
        }
        if !ran.ok() {
            return Err(Failure::new(
                step,
                Kind::Failed,
                format!("Could not install on {}: {}", self.name(), ran.last_line()),
            ));
        }
        let ran = self
            .run_words(
                step,
                &program(contract, &p, contract.version_args()),
                Input::None,
                secs(30),
            )
            .await?;
        let version = contract.version_of(&ran.stdout);
        // An empty `build.version`: any version, as long as the program runs there and says one.
        let fits = match build.version.as_str() {
            "" => version.is_some(),
            want => version.as_deref() == Some(want),
        };
        if !fits {
            return Err(Failure::new(
                step,
                Kind::Failed,
                format!(
                    "The program on {} says it is {}, not {}",
                    self.name(),
                    version.as_deref().unwrap_or("no version"),
                    build.version
                ),
            ));
        }
        Ok(true)
    }

    /// Step 4: serve on the socket, as a `systemd --user` service with lingering on when it
    /// can, else in the background with a pid file. `restart` starts it again (a new build).
    /// Waits until the socket answers; on failure, returns the service's last log lines.
    pub async fn start(
        &self,
        contract: &dyn ServeContract,
        facts: &Facts,
        restart: bool,
    ) -> Result<Started, Failure> {
        let step = Step::Start;
        let name = checked_name(contract, step)?;
        let p = Paths::new(&facts.home, &name);
        if p.socket.len() > SOCKET_MAX || p.socket.contains(':') {
            return Err(Failure::new(
                step,
                Kind::Failed,
                format!(
                    "The home folder on {} is too long for a socket: {}",
                    self.name(),
                    p.socket
                ),
            ));
        }
        let ran = self
            .script(
                step,
                "mkdir -p \"$1\" \"$2\" && chmod 700 \"$1\" \"$2\"",
                &[&p.state, &p.sock_dir],
                Input::None,
                secs(30),
            )
            .await?;
        if !ran.ok() {
            return Err(self.failed(step, "Could not start on", &ran));
        }
        let flag = if restart { "1" } else { "0" };
        let (ran, systemd) = if facts.systemd {
            let unit = unit_text(contract, &p);
            let ran = self
                .script(
                    step,
                    UNIT,
                    &[
                        p.unit_dir.as_str(),
                        p.unit.as_str(),
                        p.unit_name.as_str(),
                        flag,
                    ],
                    Input::Bytes(unit.as_bytes()),
                    secs(60),
                )
                .await?;
            (ran, true)
        } else {
            let mut args = vec![p.pid.clone(), p.log.clone(), flag.into(), p.bin.clone()];
            args.extend(program(contract, &p, contract.serve_args(&p.socket)));
            let ran = self
                .script(step, BACKGROUND, &args, Input::None, secs(30))
                .await?;
            (ran, false)
        };
        if !ran.ok() {
            let mut f = self.failed(step, "Could not start on", &ran);
            f.kind = Kind::NotStarted;
            f.log = self.service_log(&p, systemd).await;
            return Err(f);
        }
        let restarted = ran.stdout.lines().any(|l| l.trim() == "restarted");

        let ran = self
            .script(step, WAIT, &[&p.socket], Input::None, secs(60))
            .await?;
        let answer = ran.stdout.lines().last().unwrap_or("").trim().to_string();
        let Some(mode) = answer.strip_prefix("up") else {
            let mut f = Failure::new(
                step,
                Kind::NotStarted,
                format!("The service on {} did not start", self.name()),
            );
            f.log = self.service_log(&p, systemd).await;
            return Err(f);
        };
        let mode = mode.trim();
        if !mode.is_empty() && mode != "600" {
            return Err(Failure::new(
                step,
                Kind::Failed,
                format!(
                    "The service on {} left its socket open to others (mode {mode})",
                    self.name()
                ),
            ));
        }

        let mut linger = facts.linger == Some(true);
        if systemd && !linger {
            let ran = self
                .script(
                    step,
                    "loginctl enable-linger \"$1\" >/dev/null 2>&1; loginctl show-user \"$1\" -p Linger 2>/dev/null",
                    &[&facts.user],
                    Input::None,
                    secs(30),
                )
                .await?;
            linger = ran.stdout.lines().any(|l| l.trim() == "Linger=yes");
        }
        let linger_terminal = (systemd && !linger).then(|| {
            self.terminal_run(&["sudo", "loginctl", "enable-linger", facts.user.as_str()])
        });
        Ok(Started {
            socket: p.socket,
            systemd,
            linger,
            linger_terminal,
            restarted,
        })
    }

    /// Step 5: a new token named `token` (the computer's name), made by the program there.
    /// A token with that name is revoked first, so pairing again replaces it.
    pub async fn pair(&self, contract: &dyn ServeContract, token: &str) -> Result<Secret, Failure> {
        let step = Step::Pair;
        let name = checked_name(contract, step)?;
        if token.trim().is_empty() || token.chars().any(char::is_control) {
            return Err(Failure::new(
                step,
                Kind::Failed,
                "A token needs a name without line breaks",
            ));
        }
        let home = self.home(step).await?.to_string();
        let p = Paths::new(&home, &name);
        self.run_words(
            step,
            &program(contract, &p, contract.revoke_args(token)),
            Input::None,
            secs(30),
        )
        .await?;
        let ran = self
            .run_words(
                step,
                &program(contract, &p, contract.token_args(token)),
                Input::None,
                secs(30),
            )
            .await?;
        if !ran.ok() {
            return Err(Failure::new(
                step,
                Kind::Failed,
                format!("Could not pair with {}: {}", self.name(), ran.last_line()),
            ));
        }
        match contract.token_of(&ran.stdout) {
            Some(t) if !t.is_empty() => Ok(Secret::new(t)),
            _ => Err(Failure::new(
                step,
                Kind::Failed,
                format!("{} did not answer with a token", self.name()),
            )),
        }
    }

    fn failed(&self, step: Step, what: &str, ran: &crate::ssh::Ran) -> Failure {
        Failure::new(
            step,
            Kind::Failed,
            format!("{what} {}: {}", self.name(), ran.last_line()),
        )
    }

    /// The service's last 20 log lines, best effort.
    async fn service_log(&self, p: &Paths, systemd: bool) -> Vec<String> {
        let ran = if systemd {
            self.script(
                Step::Start,
                "journalctl --user -u \"$1\" -n 20 --no-pager 2>/dev/null || systemctl --user status \"$1\" --no-pager 2>&1 | tail -n 20",
                &[&p.unit_name],
                Input::None,
                secs(30),
            )
            .await
        } else {
            self.script(
                Step::Start,
                "tail -n 20 \"$1\" 2>/dev/null",
                &[&p.log],
                Input::None,
                secs(30),
            )
            .await
        };
        ran.map(|r| r.stdout.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl ServeContract for Fake {
        fn name(&self) -> &str {
            "app"
        }
        fn serve_args(&self, socket: &str) -> Vec<String> {
            vec!["serve".into(), "--socket".into(), socket.into()]
        }
        fn token_args(&self, t: &str) -> Vec<String> {
            vec!["token".into(), "new".into(), t.into()]
        }
        fn revoke_args(&self, t: &str) -> Vec<String> {
            vec!["token".into(), "revoke".into(), t.into()]
        }
        fn version_args(&self) -> Vec<String> {
            vec!["--version".into()]
        }
        fn env(&self) -> Vec<(String, String)> {
            vec![("APP_HOME".into(), "100% $HOME".into())]
        }
    }

    #[test]
    fn libc_versions_are_read() {
        assert_eq!(libc_of("glibc 2.31"), Some("glibc 2.31".into()));
        assert_eq!(
            libc_of("ldd (Debian GLIBC 2.36-9+deb12u7) 2.36"),
            Some("glibc 2.36".into())
        );
        assert_eq!(libc_of("musl libc (aarch64)"), Some("musl".into()));
        assert_eq!(libc_of("sh: ldd: not found"), None);
    }

    #[test]
    fn facts_are_read_from_the_look_output() {
        let out = "user=me\nuname=Linux aarch64\nlibc=glibc 2.36\ndisk=1048576\nsystemd=yes\n\
                   Linger=no\nsshd=allowstreamlocalforwarding yes\ninstalled=yes\n\
                   sha256=ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789  /h/x\n\
                   version-follows\napp 1.2.3\n";
        let (f, ok) = read_facts(&Fake, "/home/me", out);
        assert!(ok);
        assert_eq!(f.arch, "aarch64");
        assert_eq!(f.libc.as_deref(), Some("glibc 2.36"));
        assert_eq!(f.free_kib, Some(1048576));
        assert!(f.systemd && f.installed);
        assert_eq!(f.linger, Some(false));
        assert_eq!(f.forwarding, Some(true));
        assert_eq!(f.version.as_deref(), Some("app 1.2.3"));
        assert_eq!(
            f.sha256.as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );

        let (f, ok) = read_facts(
            &Fake,
            "/h",
            "uname=Darwin arm64\nsshd=disableforwarding yes\n",
        );
        assert!(ok);
        assert_eq!((f.os.as_str(), f.arch.as_str()), ("macos", "aarch64"));
        assert_eq!(f.forwarding, Some(false));
        assert!(!f.installed && f.version.is_none());
        let (f, _) = read_facts(&Fake, "/h", "uname=Linux x86_64\n");
        assert_eq!(f.forwarding, None);
        assert_eq!((f.os.as_str(), f.arch.as_str()), ("linux", "x86_64"));
        for unsupported in ["SunOS i86pc", "Linux riscv64", "Darwin"] {
            let (_, ok) = read_facts(&Fake, "/h", &format!("uname={unsupported}\n"));
            assert!(!ok, "{unsupported}");
        }
    }

    #[test]
    fn the_unit_quotes_for_systemd() {
        let p = Paths::new("/home/me/", "app");
        assert_eq!(p.bin, "/home/me/.local/share/app/app");
        assert_eq!(p.socket, "/home/me/.local/state/app/sock/serve.sock");
        let text = unit_text(&Fake, &p);
        assert!(text.contains(
            "ExecStart=\"/home/me/.local/share/app/app\" \"serve\" \"--socket\" \"/home/me/.local/state/app/sock/serve.sock\"\n"
        ));
        assert!(
            text.contains("Environment=\"APP_HOME=100%% $$HOME\"\n"),
            "{text}"
        );
        assert_eq!(unit_quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn names_and_stores_are_checked() {
        assert!(valid_name("ark"));
        assert!(valid_name("ark-server_2.x"));
        for bad in ["", "-x", ".x", "a b", "a/b", "a'b", "$x"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert_eq!(checked_store(".ark", Step::Remove).unwrap(), ".ark");
        assert_eq!(
            checked_store(".local/share/x/", Step::Remove).unwrap(),
            ".local/share/x"
        );
        for bad in ["/", "/etc", "", "..", "a/../..", "./", "a//b"] {
            assert!(checked_store(bad, Step::Remove).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_program_runs_with_its_environment() {
        let p = Paths::new("/h", "app");
        assert_eq!(
            program(&Fake, &p, vec!["--version".into()]),
            [
                "env",
                "APP_HOME=100% $HOME",
                "/h/.local/share/app/app",
                "--version"
            ]
        );
    }

    #[test]
    fn default_contract_readers() {
        assert_eq!(Fake.token_of("note\n\nark_abc\n\n"), Some("ark_abc".into()));
        assert_eq!(Fake.version_of("\n v1 \nmore"), Some("v1".into()));
    }
}
