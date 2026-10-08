//! A computer added over SSH, end to end (ADR 0052): an `sshd` in a container plays the person's
//! server; the real server build goes there through the person's own `ssh`; a chat placed on it
//! runs there, asks here, writes a file that comes back; the tunnel comes back after `sshd`
//! restarts; Remove revokes the token there.
//!
//! Needs Docker and a Linux server build of this runtime for the container's architecture:
//!
//! ```text
//! ARK_TEST_SSH=1 ARK_SERVER_BUILD=<ark-desktop's target/linux/ark-desktop-aarch64-unknown-linux-gnu> \
//!   cargo test -p ark-ui --test computers
//! ```
//!
//! `ARK_SERVER_KIND=writer` with the writer's build (`scripts/linux-build.sh writing-agent
//! writer`): the second app, its own contract and its own tool saving a chapter there.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use ark_interact::App;
use ark_reach::{Build, ServeContract};
use ark_sdk::prelude::ark_connect::MemorySecrets;
use ark_sdk::prelude::*;
use ark_ui::computers::ComputerSetup;
use ark_ui::{ArkUi, Host};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const IMAGE: &str = "ark-ui-test-computer";
const DOCKERFILE: &str = "FROM debian:12-slim
RUN apt-get update && apt-get install -y --no-install-recommends openssh-server procps curl ca-certificates git && rm -rf /var/lib/apt/lists/*
RUN ssh-keygen -A && mkdir -p /run/sshd
RUN useradd -m -s /bin/bash reach && usermod -p '*' reach && mkdir -p /home/reach/.ssh && chown reach:reach /home/reach/.ssh && chmod 700 /home/reach/.ssh
RUN printf 'while true; do /usr/sbin/sshd -D -e; sleep 0.3; done\\n' > /run-sshd.sh
CMD [\"sh\", \"/run-sshd.sh\"]
";

/// The model there: write a file (it asks first), then say so. The desktop app writes with
/// ARK's file tool; the writer with its own.
fn script(writer: bool) -> &'static str {
    match writer {
        false => {
            r#"[
  {"tool_calls": [{"name": "files.write", "arguments": {"path": "notes.md", "text": "from the server\n"}}]},
  {"content": "Wrote notes.md."}
]"#
        }
        true => {
            r#"[
  {"tool_calls": [{"name": "manuscript.save", "arguments": {"file": "notes.md", "text": "from the server\n"}}]},
  {"content": "Wrote notes.md."}
]"#
        }
    }
}

/// The server build, as its app's contract runs it, with the scripted model.
struct Server {
    writer: bool,
}

impl ServeContract for Server {
    fn name(&self) -> &str {
        match self.writer {
            true => "writer",
            false => "ark-desktop",
        }
    }
    fn serve_args(&self, socket: &str) -> Vec<String> {
        vec![
            "--serve".into(),
            format!("unix:{socket}"),
            "--tokens".into(),
        ]
    }
    fn token_args(&self, token: &str) -> Vec<String> {
        match self.writer {
            true => vec!["--new-token".into(), token.into()],
            false => vec!["token".into(), "create".into(), token.into()],
        }
    }
    fn revoke_args(&self, token: &str) -> Vec<String> {
        match self.writer {
            true => vec!["--revoke-token".into(), token.into()],
            false => vec!["token".into(), "revoke".into(), token.into()],
        }
    }
    fn version_args(&self) -> Vec<String> {
        vec!["--version".into()]
    }
    fn env(&self) -> Vec<(String, String)> {
        vec![("ARK_MOCK_SCRIPT".into(), "/home/reach/mock.json".into())]
    }
    fn token_of(&self, stdout: &str) -> Option<String> {
        stdout
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
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

struct Container(String);

impl Container {
    fn exec(&self, script: &str) -> String {
        docker(&["exec", &self.0, "sh", "-c", script])
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

/// The container, a throwaway key in it, and an `ssh` config naming it `reachtest`.
fn rig(dir: &Path, writer: bool) -> (Container, PathBuf) {
    let ctx = dir.join("image");
    std::fs::create_dir(&ctx).unwrap();
    std::fs::write(ctx.join("Dockerfile"), DOCKERFILE).unwrap();
    docker(&["build", "-q", "-t", IMAGE, ctx.to_str().unwrap()]);
    let c = Container(
        docker(&["run", "-d", "-p", "127.0.0.1::22", IMAGE])
            .trim()
            .to_string(),
    );
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
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(dir.join("key")));
    let key = std::fs::read_to_string(dir.join("key.pub")).unwrap();
    c.exec(&format!(
        "printf '%s' '{}' > /home/reach/.ssh/authorized_keys && chown reach:reach /home/reach/.ssh/authorized_keys && chmod 600 /home/reach/.ssh/authorized_keys",
        key.trim()
    ));
    c.exec(&format!(
        "cat > /home/reach/mock.json <<'EOF'\n{}\nEOF\nchown reach:reach /home/reach/mock.json",
        script(writer)
    ));
    // The host key, as the person would have accepted it once.
    let host_key = c.exec("cut -d ' ' -f 1,2 /etc/ssh/ssh_host_ed25519_key.pub");
    std::fs::write(
        dir.join("known_hosts"),
        format!("[127.0.0.1]:{port} {}\n", host_key.trim()),
    )
    .unwrap();
    let d = dir.display();
    let cfg = dir.join("config");
    std::fs::write(
        &cfg,
        format!(
            "Host reachtest\n  HostName 127.0.0.1\n  Port {port}\n  User reach\n  IdentityFile {d}/key\n  IdentitiesOnly yes\n  IdentityAgent none\n  UserKnownHostsFile {d}/known_hosts\n  GlobalKnownHostsFile /dev/null\n"
        ),
    )
    .unwrap();
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
            return (c, cfg);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("sshd did not come up");
}

fn app(dir: &Path) -> App {
    let provider: Arc<dyn ark_model::Provider> =
        Arc::new(ark_model::MockProvider::new("m", vec![]));
    let ark = Ark::builder()
        .store_root(dir)
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    App::new("Test").ark(ark)
}

async fn until(what: &str, secs: u64, mut f: impl AsyncFnMut() -> bool) {
    for _ in 0..secs * 10 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("never: {what}");
}

/// What a chat's events were, for a failure's message.
fn told(seen: &std::sync::Mutex<Vec<Value>>, session: &str) -> String {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|v| v["session"] == session)
        .map(|v| v.to_string().chars().take(300).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn record(host: &Arc<Host>) -> Arc<std::sync::Mutex<Vec<Value>>> {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (mut rx, keep) = (host.subscribe(), seen.clone());
    tokio::spawn(async move {
        while let Ok(v) = rx.recv().await {
            keep.lock().unwrap().push(v);
        }
    });
    seen
}

fn any(seen: &std::sync::Mutex<Vec<Value>>, f: impl Fn(&Value) -> bool) -> Option<Value> {
    seen.lock().unwrap().iter().find(|v| f(v)).cloned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_runs_on_a_computer_added_over_ssh() {
    if std::env::var_os("ARK_TEST_SSH").is_none() {
        eprintln!("skipped: set ARK_TEST_SSH=1 (needs Docker) and ARK_SERVER_BUILD");
        return;
    }
    let build = PathBuf::from(std::env::var("ARK_SERVER_BUILD").expect("ARK_SERVER_BUILD"));
    let sha256 = Sha256::digest(std::fs::read(&build).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let (tmp, dir_a, work) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let writer = std::env::var("ARK_SERVER_KIND").as_deref() == Ok("writer");
    let (c, cfg) = rig(tmp.path(), writer);
    std::fs::write(work.path().join("README.md"), "hello\n").unwrap();
    let b = Build {
        path: build,
        sha256,
        version: String::new(),
    };
    let (a, _) = ArkUi::new(app(dir_a.path()))
        .computers(ComputerSetup {
            contract: Arc::new(Server { writer }),
            build: Arc::new(move |_| Ok(b.clone())),
            secrets: Arc::new(MemorySecrets::default()),
            ssh_config: Some(cfg),
        })
        .open()
        .await
        .unwrap();
    let seen = record(&a);
    a.call(
        "add_project",
        json!({ "name": "site", "path": work.path() }),
    )
    .await
    .unwrap();

    // Settings › Computers: the host from the config, set up step by step.
    let hosts = a.call("computer_hosts", json!({})).await.unwrap();
    assert!(hosts.to_string().contains("reachtest"), "{hosts}");
    a.call("computer_add", json!({ "target": "reachtest" }))
        .await
        .unwrap();
    until("the setup", 240, async || {
        any(&seen, |v| {
            v["kind"] == "computer_setup" && (v["done"] == true || !v["failure"].is_null())
        })
        .is_some()
    })
    .await;
    let end = any(&seen, |v| {
        v["kind"] == "computer_setup" && (v["done"] == true || !v["failure"].is_null())
    })
    .unwrap();
    assert_eq!(end["done"], true, "{end}");
    let steps: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|v| v["step"].as_str().map(str::to_string))
        .collect();
    for s in ["connect", "look", "install", "start", "pair", "tunnel"] {
        assert!(steps.iter().any(|x| x == s), "{s} in {steps:?}");
    }
    until("the link", 30, async || {
        a.links().state("reachtest") == Some(ark_interact::links::State::Ok)
    })
    .await;
    let places = a.call("places", json!({})).await.unwrap();
    assert_eq!(places["places"][0]["name"], "reachtest", "{places}");
    assert_eq!(places["can_add"], true);

    // A chat there: the project goes, the turn runs there and asks here.
    let placed = a
        .call(
            "new_session",
            json!({ "place": "reachtest", "model": "mock:script" }),
        )
        .await
        .unwrap();
    assert_eq!(placed["copy"]["sent"], 1, "{placed}");
    let s = placed["session"]["id"].as_str().unwrap().to_string();
    a.call("send", json!({ "text": "write the notes" }))
        .await
        .unwrap();
    for _ in 0..600 {
        if any(&seen, |v| {
            v["kind"] == "approval" && v["session"] == s.as_str()
        })
        .is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        any(&seen, |v| v["kind"] == "approval"
            && v["session"] == s.as_str())
        .is_some(),
        "no question; the chat said:\n{}",
        told(&seen, &s)
    );
    let q = any(&seen, |v| {
        v["kind"] == "approval" && v["session"] == s.as_str()
    })
    .unwrap();

    // sshd goes away and comes back while the question waits: the tunnel comes back, and the
    // question is still there to answer.
    c.exec("pkill -x sshd || true");
    until("the link to come back", 90, async || {
        a.links().state("reachtest") == Some(ark_interact::links::State::Ok)
            && a.call("hello", json!({})).await.unwrap()["approvals"]
                .to_string()
                .contains(q["id"].as_str().unwrap())
    })
    .await;
    a.call(
        "approve",
        json!({ "id": q["id"], "decision": "allow_once" }),
    )
    .await
    .unwrap();
    until("the turn", 60, async || {
        any(&seen, |v| {
            v["kind"] == "turn_finished" && v["session"] == s.as_str()
        })
        .is_some()
    })
    .await;
    let text: String = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "text" && v["session"] == s.as_str())
        .map(|v| v["text"].as_str().unwrap_or("").to_string())
        .collect();
    let transcript = a.call("transcript", json!({})).await.unwrap().to_string();
    assert!(
        text.contains("Wrote notes.md") || transcript.contains("Wrote notes.md"),
        "{text:?} {transcript}"
    );

    // Its file comes back.
    let c2 = a.call("place_changes", json!({})).await.unwrap();
    assert_eq!(c2["count"], 1, "{c2}");
    a.call("place_bring", json!({})).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(work.path().join("notes.md")).unwrap(),
        "from the server\n"
    );

    // Remove: the token goes there, the link and its chats here.
    a.call("computer_remove", json!({ "name": "reachtest" }))
        .await
        .unwrap();
    if !writer {
        let left = c.exec(
            "su reach -c '~/.local/share/ark-desktop/ark-desktop token list 2>/dev/null || true'",
        );
        assert!(!left.contains("ark-"), "the token was revoked: {left}");
    }
    assert!(a.links().state("reachtest").is_none());
    let rows = a.call("sessions", json!({ "all": true })).await.unwrap();
    assert!(!rows.to_string().contains(&s), "{rows}");
}
