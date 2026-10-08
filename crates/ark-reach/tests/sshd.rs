//! Reach against a real `sshd` in a container (Debian 12). Runs only with `ARK_TEST_SSH=1`
//! and Docker; skipped otherwise.
//!
//! Everything lives in a temporary folder: a throwaway key pair, `known_hosts` and SSH config,
//! passed with `-F`. The person's `~/.ssh` is never read or written. The "build" is a small
//! Python script with a fake contract (serve HTTP on a Unix socket, make and revoke tokens,
//! print a version), so no Linux build of ARK is needed.
//!
//! The container has no systemd, so the Start step takes its fallback (a background process
//! with a pid file) and reports lingering as off.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use ark_reach::{
    hosts, remove, setup, Build, Kind, Plan, ServeContract, Session, Ssh, Step, Target, Tunnel,
    TunnelState,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const IMAGE: &str = "ark-reach-test-sshd";

const DOCKERFILE: &str = "FROM debian:12-slim
RUN apt-get update && apt-get install -y --no-install-recommends openssh-server python3 procps curl && rm -rf /var/lib/apt/lists/*
RUN ssh-keygen -A && mkdir -p /run/sshd
RUN useradd -m -s /bin/bash reach && usermod -p '*' reach && mkdir -p /home/reach/.ssh && chown reach:reach /home/reach/.ssh && chmod 700 /home/reach/.ssh
RUN printf 'while true; do /usr/sbin/sshd -D -e; sleep 0.3; done\\n' > /run-sshd.sh
CMD [\"/bin/sh\", \"/run-sshd.sh\"]
";

const SYSTEMD_IMAGE: &str = "ark-reach-test-systemd";

/// systemd as the container's init, so ssh logins get a `systemd --user` manager. No polkit,
/// so the person cannot turn lingering on themselves.
const SYSTEMD_DOCKERFILE: &str = "FROM debian:12-slim
RUN apt-get update && apt-get install -y --no-install-recommends systemd systemd-sysv dbus dbus-user-session libpam-systemd openssh-server python3 procps curl && rm -rf /var/lib/apt/lists/*
RUN ssh-keygen -A && systemctl enable ssh
RUN useradd -m -s /bin/bash reach && usermod -p '*' reach && mkdir -p /home/reach/.ssh && chown reach:reach /home/reach/.ssh && chmod 700 /home/reach/.ssh
STOPSIGNAL SIGRTMIN+3
CMD [\"/sbin/init\"]
";

const FAKE: &str = r#"#!/usr/bin/env python3
import hashlib, http.server, os, secrets, socketserver, sys
VERSION = "fake VERSION_HERE"
STORE = os.path.join(os.path.expanduser("~"), ".fake-store")
TOKENS = os.path.join(STORE, "tokens")
def rows():
    try:
        return [l.split() for l in open(TOKENS).read().splitlines() if l.strip()]
    except FileNotFoundError:
        return []
def save(rs):
    os.makedirs(STORE, exist_ok=True)
    with open(TOKENS, "w") as f:
        f.write("".join(n + " " + h + "\n" for n, h in rs))
def sha(t):
    return hashlib.sha256(t.encode()).hexdigest()
a = sys.argv[1:]
if a == ["version"]:
    print(VERSION)
elif a[:2] == ["token", "new"]:
    if any(n == a[2] for n, _ in rows()):
        sys.exit("a token named " + a[2] + " exists")
    t = "fake_" + secrets.token_hex(16)
    save(rows() + [[a[2], sha(t)]])
    print(t)
elif a[:2] == ["token", "revoke"]:
    save([r for r in rows() if r[0] != a[2]])
elif a[:1] == ["serve"]:
    class H(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            tok = self.headers.get("Authorization", "").removeprefix("Bearer ")
            ok = any(h == sha(tok) for _, h in rows())
            body = (os.environ.get("FAKE_GREETING", "") + (" yes" if ok else " no")).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        def log_message(self, *args):
            pass
    try:
        os.unlink(a[1])
    except FileNotFoundError:
        pass
    os.umask(0o177)
    socketserver.ThreadingUnixStreamServer(a[1], H).serve_forever()
else:
    sys.exit("unknown command")
"#;

struct Fake;

impl ServeContract for Fake {
    fn name(&self) -> &str {
        "fake"
    }
    fn serve_args(&self, socket: &str) -> Vec<String> {
        vec!["serve".into(), socket.into()]
    }
    fn token_args(&self, token: &str) -> Vec<String> {
        vec!["token".into(), "new".into(), token.into()]
    }
    fn revoke_args(&self, token: &str) -> Vec<String> {
        vec!["token".into(), "revoke".into(), token.into()]
    }
    fn version_args(&self) -> Vec<String> {
        vec!["version".into()]
    }
    fn env(&self) -> Vec<(String, String)> {
        vec![("FAKE_GREETING".into(), "hello".into())]
    }
    fn store(&self) -> Option<String> {
        Some(".fake-store".into())
    }
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.stderr(Stdio::inherit()).output().unwrap();
    assert!(out.status.success(), "{cmd:?} failed");
    String::from_utf8(out.stdout).unwrap()
}

fn docker(args: &[&str]) -> String {
    run(Command::new("docker").args(args))
}

/// Removes the container when the test ends, passed or not.
struct Container(String);

impl Container {
    fn exec(&self, script: &str) -> String {
        docker(&["exec", &self.0, "sh", "-c", script])
    }
    fn ok(&self, script: &str) -> bool {
        Command::new("docker")
            .args(["exec", &self.0, "sh", "-c", script])
            .status()
            .unwrap()
            .success()
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.0])
            .stdout(Stdio::null())
            .status();
    }
}

fn config(dir: &Path, name: &str, port: &str, known_hosts: &str) -> PathBuf {
    let d = dir.display();
    let text = format!(
        "Include {d}/conf.d/*.conf\n\
         Host reachtest\n  HostName 127.0.0.1\n  Port {port}\n  User reach\n\
         Host *.wild other-?\n  User nobody\n\
         Host *\n  IdentityFile {d}/key\n  IdentitiesOnly yes\n  IdentityAgent none\n\
         \x20 UserKnownHostsFile {d}/{known_hosts}\n  GlobalKnownHostsFile /dev/null\n"
    );
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn build(dir: &Path, version: &str) -> Build {
    let path = dir.join(format!("fake-{version}"));
    std::fs::write(&path, FAKE.replace("VERSION_HERE", version)).unwrap();
    let sha256 = Sha256::digest(std::fs::read(&path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Build {
        path,
        sha256,
        version: format!("fake {version}"),
    }
}

/// GET / over a Unix socket; the response body.
async fn get(socket: &Path, token: Option<&str>) -> String {
    let mut s = tokio::net::UnixStream::connect(socket).await.unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    s.write_all(format!("GET / HTTP/1.0\r\nHost: x\r\n{auth}\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out.split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or(out)
}

async fn wait_state(
    tunnel: &Tunnel,
    limit: u64,
    want: impl Fn(&TunnelState) -> bool,
) -> TunnelState {
    let mut rx = tunnel.state();
    let s = tokio::time::timeout(Duration::from_secs(limit), rx.wait_for(|s| want(s)))
        .await
        .unwrap_or_else(|_| panic!("tunnel state stayed {:?}", *tunnel.state().borrow()))
        .unwrap()
        .clone();
    s
}

/// Polls `script` in the container until it succeeds, for up to `secs`.
async fn eventually(c: &Container, script: &str, secs: u64) -> bool {
    for _ in 0..secs * 2 {
        if c.ok(script) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

/// A container from `dockerfile`, a throwaway key in it, and SSH configs in a temporary
/// folder: `config` (the right host key), `config_empty` (none), `config_wrong` (another).
struct Rig {
    tmp: tempfile::TempDir,
    c: Container,
    port: String,
    cfg: PathBuf,
}

async fn rig(tag: &str, dockerfile: &str, run_args: &[&str]) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // The image and the container.
    let ctx = dir.join("image");
    std::fs::create_dir(&ctx).unwrap();
    std::fs::write(ctx.join("Dockerfile"), dockerfile).unwrap();
    docker(&["build", "-q", "-t", tag, ctx.to_str().unwrap()]);
    let mut args = vec!["run", "-d", "-p", "127.0.0.1::22"];
    args.extend(run_args);
    args.push(tag);
    let id = docker(&args);
    let c = Container(id.trim().to_string());
    let port = docker(&["port", &c.0, "22/tcp"]);
    let port = port
        .lines()
        .next()
        .unwrap()
        .rsplit(':')
        .next()
        .unwrap()
        .trim()
        .to_string();

    // A throwaway key, known_hosts (right, empty, wrong) and configs.
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(dir.join("key")));
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(dir.join("other")));
    let mut put = Command::new("docker")
        .args(["exec", "-i", &c.0, "sh", "-c"])
        .arg("cat > /home/reach/.ssh/authorized_keys && chown reach:reach /home/reach/.ssh/authorized_keys && chmod 600 /home/reach/.ssh/authorized_keys")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        put.stdin.as_mut().unwrap(),
        &std::fs::read(dir.join("key.pub")).unwrap(),
    )
    .unwrap();
    drop(put.stdin.take());
    assert!(put.wait().unwrap().success());
    let host_key = c.exec("cut -d ' ' -f 1,2 /etc/ssh/ssh_host_ed25519_key.pub");
    let other_key = std::fs::read_to_string(dir.join("other.pub")).unwrap();
    let other_key: Vec<&str> = other_key.split_whitespace().take(2).collect();
    let at = format!("[127.0.0.1]:{port}");
    std::fs::write(
        dir.join("known_hosts"),
        format!("{at} {}\n", host_key.trim()),
    )
    .unwrap();
    std::fs::write(dir.join("known_hosts_empty"), "").unwrap();
    std::fs::write(
        dir.join("known_hosts_wrong"),
        format!("{at} {}\n", other_key.join(" ")),
    )
    .unwrap();
    std::fs::create_dir(dir.join("conf.d")).unwrap();
    std::fs::write(
        dir.join("conf.d/extra.conf"),
        format!("Host reach-alias\n  HostName 127.0.0.1\n  Port {port}\n  User reach\n"),
    )
    .unwrap();
    let cfg = config(dir, "config", &port, "known_hosts");
    config(dir, "config_empty", &port, "known_hosts_empty");
    config(dir, "config_wrong", &port, "known_hosts_wrong");

    // Wait for sshd.
    let mut up = false;
    for _ in 0..60 {
        let ok = Command::new("ssh")
            .arg("-F")
            .arg(&cfg)
            .args(["-o", "BatchMode=yes", "--", "reachtest", "true"])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        if ok {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(up, "sshd did not come up");
    Rig { tmp, c, port, cfg }
}

#[tokio::test(flavor = "multi_thread")]
async fn reach_against_sshd() {
    if std::env::var("ARK_TEST_SSH").as_deref() != Ok("1") {
        return;
    }
    let Rig { tmp, c, port, cfg } = rig(IMAGE, DOCKERFILE, &["--init"]).await;
    let dir = tmp.path();
    let cfg_empty = dir.join("config_empty");
    let cfg_wrong = dir.join("config_wrong");

    // Hosts from the config, through Include, without patterns, resolved by ssh -G.
    let found = hosts(Some(&cfg)).await;
    let names: Vec<&str> = found.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["reach-alias", "reachtest"]);
    assert_eq!(found[0].user, "reach");
    assert_eq!(found[0].hostname, "127.0.0.1");
    assert_eq!(found[0].port.to_string(), port);

    // Targets that would be options or shell words are refused before ssh runs.
    for bad in [
        "-oProxyCommand=touch /tmp/x",
        "-oProxyCommand=x",
        "a'b",
        "reachtest;id",
        "a@b@c",
    ] {
        assert!(Target::parse(bad).is_err(), "{bad}");
    }

    let ssh = Ssh::system(Some(cfg.clone())).expect("ssh");
    let target = Target::parse("reachtest").unwrap();
    let state = dir.join("state");
    let v1 = build(dir, "1.0");
    let contract = Fake;

    // An unknown host key: the person answers ssh once in a terminal.
    let s = Session::new(
        Ssh::system(Some(cfg_empty)).unwrap(),
        target.clone(),
        &state,
    )
    .unwrap();
    let plan = Plan {
        build: &v1,
        token: Some("laptop"),
    };
    let f = setup(&s, &contract, plan, |_| {}).await.unwrap_err();
    assert_eq!(
        (f.step, &f.kind),
        (Step::Connect, &Kind::NeedsTerminal),
        "{f}"
    );
    let term = f.terminal.clone().unwrap();
    assert_eq!(&term[term.len() - 3..], ["--", "reachtest", "true"]);
    assert!(f.words.starts_with("Could not reach reachtest: "), "{f}");

    // A changed host key stops setup.
    let s = Session::new(
        Ssh::system(Some(cfg_wrong)).unwrap(),
        target.clone(),
        &state,
    )
    .unwrap();
    let f = setup(&s, &contract, plan, |_| {}).await.unwrap_err();
    assert_eq!(
        (f.step, &f.kind),
        (Step::Connect, &Kind::HostKeyChanged),
        "{f}"
    );
    assert!(c.ok("! test -e /home/reach/.local"));

    let session = Session::new(ssh.clone(), target.clone(), &state).unwrap();

    // A build with the wrong hash is refused and nothing is sent.
    let mut wrong = v1.clone();
    wrong.sha256 = "0".repeat(64);
    let f = setup(
        &session,
        &contract,
        Plan {
            build: &wrong,
            token: Some("laptop"),
        },
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(
        (f.step, &f.kind),
        (Step::Install, &Kind::BuildMismatch),
        "{f}"
    );
    assert_eq!(f.words, "The build did not match; nothing was installed");
    assert!(c.ok("! test -e /home/reach/.local/share/fake"));

    // Setup from nothing reaches Pair and returns a token.
    let mut steps = Vec::new();
    let done = setup(&session, &contract, plan, |s| steps.push(s))
        .await
        .unwrap();
    assert_eq!(
        steps,
        [
            Step::Connect,
            Step::Look,
            Step::Install,
            Step::Start,
            Step::Pair
        ]
    );
    assert!(done.installed);
    assert!(matches!(done.facts.arch.as_str(), "x86_64" | "aarch64"));
    assert_eq!(done.facts.libc.as_deref(), Some("glibc 2.36"));
    assert!(done.facts.free_kib.unwrap_or(0) > 0);
    assert!(!done.facts.installed && !done.facts.systemd);
    assert_eq!(done.host.port.to_string(), port);
    // No systemd here: started in the background, and it would stop at logout.
    assert!(!done.started.systemd && !done.started.linger && done.started.restarted);
    assert_eq!(
        done.started.socket,
        "/home/reach/.local/state/fake/sock/serve.sock"
    );
    let token = done.token.expect("a token");
    assert!(token.expose().starts_with("fake_"));
    assert!(!format!("{token:?} {token}").contains(token.expose()));
    assert_eq!(
        c.exec("stat -c %a /home/reach/.local/state/fake/sock/serve.sock")
            .trim(),
        "600"
    );
    assert_eq!(
        c.exec("stat -c %a /home/reach/.local/state/fake/sock")
            .trim(),
        "700"
    );
    assert_eq!(
        c.exec("stat -c %a /home/reach/.local/share/fake/fake")
            .trim(),
        "755"
    );
    eprintln!("facts: {:?}", done.facts);

    // Setup again only checks.
    let mut steps = Vec::new();
    let again = setup(
        &session,
        &contract,
        Plan {
            build: &v1,
            token: None,
        },
        |s| steps.push(s),
    )
    .await
    .unwrap();
    assert_eq!(
        steps,
        [Step::Connect, Step::Look, Step::Install, Step::Start]
    );
    assert!(!again.installed && !again.started.restarted && again.token.is_none());
    assert!(again.facts.installed);
    assert_eq!(again.facts.version.as_deref(), Some("fake 1.0"));
    assert_eq!(again.facts.sha256.as_deref(), Some(v1.sha256.as_str()));

    // A new version replaces the program, keeps the one before, and restarts the service.
    let v2 = build(dir, "2.0");
    let up2 = setup(
        &session,
        &contract,
        Plan {
            build: &v2,
            token: None,
        },
        |_| {},
    )
    .await
    .unwrap();
    assert!(up2.installed && up2.started.restarted);
    assert!(c.ok("grep -q 'fake 1.0' /home/reach/.local/share/fake/fake.previous"));

    // The tunnel answers, with the token and the contract's environment.
    let tunnel = Tunnel::open(&ssh, &target, &dir.join("t"), &done.started.socket).unwrap();
    tunnel.wait_up(Duration::from_secs(30)).await.unwrap();
    assert_eq!(
        get(tunnel.socket(), Some(token.expose())).await,
        "hello yes"
    );
    assert_eq!(get(tunnel.socket(), Some("fake_wrong")).await, "hello no");

    // sshd restarts (every sshd process ends): the tunnel goes down and comes back.
    c.exec("pkill -x sshd || true");
    let s = wait_state(&tunnel, 30, |s| *s != TunnelState::Up).await;
    eprintln!("after sshd restart: {s:?}");
    wait_state(&tunnel, 60, |s| *s == TunnelState::Up).await;
    assert_eq!(
        get(tunnel.socket(), Some(token.expose())).await,
        "hello yes"
    );

    // A socket that is not there is "not answering", not a refusal.
    let none = Tunnel::open(&ssh, &target, &dir.join("t"), "/home/reach/none.sock").unwrap();
    let why = none.wait_up(Duration::from_secs(30)).await.unwrap_err();
    assert!(why.starts_with("reachtest is not answering"), "{why}");
    drop(none);

    // An sshd that refuses socket forwarding: the tunnel says so in the ADR's words.
    c.exec("echo 'AllowStreamLocalForwarding no' > /etc/ssh/sshd_config.d/no-forwarding.conf && (pkill -x sshd || true)");
    let s = wait_state(
        &tunnel,
        60,
        |s| matches!(s, TunnelState::Down(w) if w.contains("forwarding")),
    )
    .await;
    assert_eq!(
        s,
        TunnelState::Down("reachtest does not allow forwarding (sshd_config)".into())
    );
    drop(tunnel);
    c.exec("rm /etc/ssh/sshd_config.d/no-forwarding.conf && (pkill -x sshd || true)");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Remove revokes the token, stops the service and deletes the program; the store stays.
    let session = Session::new(ssh.clone(), target.clone(), &state).unwrap();
    remove(&session, &contract, Some("laptop"), false)
        .await
        .unwrap();
    assert!(c.ok("! grep -q laptop /home/reach/.fake-store/tokens"));
    assert!(c.ok("test -f /home/reach/.fake-store/tokens"));
    assert!(c.ok("! test -e /home/reach/.local/share/fake"));
    assert!(c.ok("! test -e /home/reach/.local/state/fake"));
    assert!(c.ok("! pgrep -f '[f]ake serve' >/dev/null"));
    // Asked to, it deletes the store too.
    remove(&session, &contract, None, true).await.unwrap();
    assert!(c.ok("! test -e /home/reach/.fake-store"));
    session.close().await;
}

/// The `systemd --user` path: a service, lingering refused and reported, the service stopping
/// at logout without lingering and staying with it, and Remove disabling it.
#[tokio::test(flavor = "multi_thread")]
async fn a_systemd_user_service() {
    if std::env::var("ARK_TEST_SSH").as_deref() != Ok("1") {
        return;
    }
    let systemd = [
        "--privileged",
        "--cgroupns=host",
        "-v",
        "/sys/fs/cgroup:/sys/fs/cgroup:rw",
    ];
    let Rig { tmp, c, cfg, .. } = rig(SYSTEMD_IMAGE, SYSTEMD_DOCKERFILE, &systemd).await;
    let dir = tmp.path();
    let ssh = Ssh::system(Some(cfg)).unwrap();
    let target = Target::parse("reachtest").unwrap();
    let state = dir.join("state");
    let v1 = build(dir, "1.0");
    let unit = "/home/reach/.config/systemd/user/fake.service";

    let session = Session::new(ssh.clone(), target.clone(), &state).unwrap();
    let plan = Plan {
        build: &v1,
        token: Some("laptop"),
    };
    let done = setup(&session, &Fake, plan, |_| {}).await.unwrap();
    assert!(done.facts.systemd);
    assert!(done.started.systemd && done.started.restarted);
    assert!(c.ok(&format!("grep -q 'FAKE_GREETING=hello' {unit}")));
    // The person may not turn lingering on: reported, with the sudo command for a terminal.
    assert_eq!(done.facts.linger, Some(false));
    assert!(!done.started.linger);
    let term = done
        .started
        .linger_terminal
        .clone()
        .expect("a linger command");
    assert!(term.contains(&"-t".to_string()));
    assert_eq!(
        &term[term.len() - 3..],
        [
            "--",
            "reachtest",
            "'sudo' 'loginctl' 'enable-linger' 'reach'"
        ]
    );
    let token = done.token.unwrap();

    let tunnel = Tunnel::open(&ssh, &target, &dir.join("t"), &done.started.socket).unwrap();
    tunnel.wait_up(Duration::from_secs(30)).await.unwrap();
    assert_eq!(
        get(tunnel.socket(), Some(token.expose())).await,
        "hello yes"
    );
    drop(tunnel);

    let again = setup(
        &session,
        &Fake,
        Plan {
            build: &v1,
            token: None,
        },
        |_| {},
    )
    .await
    .unwrap();
    assert!(!again.installed && !again.started.restarted);

    // Without lingering, the service stops when the person's last session ends.
    session.close().await;
    assert!(
        eventually(&c, "! pgrep -f '[f]ake serve' >/dev/null", 40).await,
        "the service kept running after logout"
    );

    // With lingering on (what `sudo loginctl enable-linger` does), it runs after logout.
    c.exec("loginctl enable-linger reach");
    let session = Session::new(ssh.clone(), target.clone(), &state).unwrap();
    let lingering = setup(
        &session,
        &Fake,
        Plan {
            build: &v1,
            token: None,
        },
        |_| {},
    )
    .await
    .unwrap();
    assert!(lingering.started.linger && lingering.started.linger_terminal.is_none());
    session.close().await;
    tokio::time::sleep(Duration::from_secs(15)).await;
    assert!(
        c.ok("pgrep -f '[f]ake serve' >/dev/null"),
        "stopped at logout with lingering on"
    );

    // Remove revokes the token, stops and disables the service.
    let session = Session::new(ssh, target, &state).unwrap();
    remove(&session, &Fake, Some("laptop"), false)
        .await
        .unwrap();
    assert!(c.ok(&format!("! test -e {unit}")));
    assert!(c.ok("! grep -q laptop /home/reach/.fake-store/tokens"));
    assert!(eventually(&c, "! pgrep -f '[f]ake serve' >/dev/null", 20).await);
    session.close().await;
}
