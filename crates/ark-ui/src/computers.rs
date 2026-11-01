//! The person's own computers, reached over SSH (ADR 0052, Reach). Adding one sets it up with
//! `ark-reach` (connect, look, install the app's build, start it as a service, pair), keeps a
//! tunnel to it, and links it ([`ark_interact::links`]): its name becomes a place a chat can run.
//!
//! | Kept | Where |
//! |---|---|
//! | a record per computer: name, `user@host`, its socket there | the store (`computer`) |
//! | its token | the app's [`Secrets`] (the keychain on a Mac), never on screen |
//! | the tunnel's sockets and `ssh`'s control sockets | a short 0700 folder of this user's |
//!
//! The app supplies what to install: a [`ServeContract`] and the build for a computer's
//! system ([`ComputerSetup`]). ARK never names the program.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_interact::ark_sdk::prelude::ark_connect::Secrets;
use ark_interact::links::{Link, Links};
pub use ark_reach;
use ark_reach::{Build, Facts, Plan, ServeContract, Session, Ssh, Target, Tunnel};
use ark_sdk::prelude::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Whether the store at `store_root` keeps the computer `name`, or kept it and the person
/// removed it since: an app that moved its own copy here ([`crate::ArkUi::keep_computer`])
/// forgets that copy then, and never moves it again.
pub fn kept(store_root: &std::path::Path, name: &str) -> ark_store::Result<bool> {
    let store = ark_store::Store::open(store_root)?;
    Ok(store.get_record(KIND, name)?.is_some() || store.get_record(REMOVED, name)?.is_some())
}

/// Computers the person removed, by name.
const REMOVED: &str = "computer_removed";

/// The build for a computer, from what Look found there (its system and architecture).
pub type BuildFor = Arc<dyn Fn(&Facts) -> anyhow::Result<Build> + Send + Sync>;

/// What an app gives so the person can add computers.
#[derive(Clone)]
pub struct ComputerSetup {
    pub contract: Arc<dyn ServeContract>,
    pub build: BuildFor,
    /// Where tokens are kept.
    pub secrets: Arc<dyn Secrets>,
    /// The `ssh` config to read hosts from and connect with; `None`: the person's own.
    pub ssh_config: Option<PathBuf>,
}

const KIND: &str = "computer";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    name: String,
    /// `user@host` or a host from `~/.ssh/config`.
    #[serde(default)]
    target: String,
    /// The socket it serves on there.
    #[serde(default)]
    socket: String,
    /// It keeps running after the person logs out there.
    #[serde(default)]
    linger: bool,
    /// The token's name there.
    #[serde(default)]
    token: String,
    /// A computer reached by its address (`https://…`), not over SSH: no tunnel, nothing to
    /// install. Its token is a session it gave for a code.
    #[serde(default)]
    url: Option<String>,
}

pub(crate) struct Computers {
    setup: ComputerSetup,
    engine: Arc<Engine>,
    links: Arc<Links>,
    /// Short enough for a Unix socket path: `ssh`'s control sockets and the tunnels' ends.
    dir: PathBuf,
    tunnels: Mutex<HashMap<String, Tunnel>>,
    /// Setups running, by target.
    busy: Mutex<std::collections::HashSet<String>>,
    /// The last command a step left for the person to run in a terminal, by target: never
    /// one a client sends.
    terminals: Mutex<HashMap<String, Vec<String>>>,
}

fn secret_name(name: &str) -> String {
    format!("computer:{name}")
}

/// A folder of this user's only, with a short path: socket paths are limited to about 100
/// bytes, which a store under `~/Library/Application Support` can exceed.
fn short_dir() -> PathBuf {
    #[cfg(unix)]
    {
        // The person's own id, from their home folder. `Computers::dir` refuses the folder
        // when another user made it first.
        use std::os::unix::fs::MetadataExt;
        let uid = ark_os::dirs::home()
            .and_then(|h| std::fs::metadata(h).ok())
            .map(|m| m.uid())
            .unwrap_or(0);
        PathBuf::from(format!("/tmp/ark-{uid}"))
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join("ark-reach")
    }
}

impl Computers {
    pub(crate) fn new(setup: ComputerSetup, engine: Arc<Engine>, links: Arc<Links>) -> Arc<Self> {
        Arc::new(Computers {
            setup,
            engine,
            links,
            dir: short_dir(),
            tunnels: Mutex::new(HashMap::new()),
            busy: Mutex::new(Default::default()),
            terminals: Mutex::new(HashMap::new()),
        })
    }

    /// A folder of `dir`, checked first: this user's own, 0700, not a link, so another user
    /// of this computer cannot swap the sockets in it.
    fn private(&self, sub: &str) -> std::io::Result<PathBuf> {
        ark_reach::private_dir(&self.dir)?;
        let d = self.dir.join(sub);
        ark_reach::private_dir(&d)?;
        Ok(d)
    }

    fn records(&self) -> Vec<Record> {
        self.engine
            .store
            .list_records(KIND, None)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect()
    }

    fn record(&self, name: &str) -> Option<Record> {
        self.records().into_iter().find(|r| r.name == name)
    }

    /// The computers added, each with how its link is and whether it stops at logout.
    pub(crate) fn list(&self) -> Vec<Value> {
        let busy = self.busy.lock().unwrap().clone();
        self.records()
            .into_iter()
            .map(|r| {
                json!({
                    "name": r.name,
                    "target": r.target,
                    "state": self.links.state(&r.name),
                    "linger": r.linger,
                    "busy": busy.contains(&r.target),
                    // Reached at its address: its sign-in there stays until signed out there.
                    "address": r.url.is_some(),
                })
            })
            .collect()
    }

    /// The hosts of `~/.ssh/config` not added yet.
    pub(crate) async fn hosts(&self) -> Vec<Value> {
        let added: Vec<String> = self.records().into_iter().map(|r| r.target).collect();
        ark_reach::hosts(self.setup.ssh_config.as_deref())
            .await
            .into_iter()
            .filter(|h| !added.contains(&h.name))
            .map(|h| json!({ "name": h.name, "user": h.user, "hostname": h.hostname }))
            .collect()
    }

    /// Open the tunnels of the computers added before, and link them (at start).
    pub(crate) fn start(self: &Arc<Self>) {
        for r in self.records() {
            if let Err(e) = self.connect(&r) {
                tracing::warn!("computer {}: {e:#}", r.name);
            }
        }
    }

    /// Keep a tunnel to `r` and link it with its token.
    fn connect(&self, r: &Record) -> anyhow::Result<()> {
        if let Some(url) = &r.url {
            let token = self
                .setup
                .secrets
                .get(&secret_name(&r.name))?
                .ok_or_else(|| anyhow::anyhow!("no token kept for {}", r.name))?;
            self.links
                .add(Link::new(r.name.clone(), url.clone(), token));
            return Ok(());
        }
        let ssh = Ssh::system(self.setup.ssh_config.clone())
            .ok_or_else(|| anyhow::anyhow!("this computer has no ssh"))?;
        let target = Target::parse(&r.target).map_err(anyhow::Error::msg)?;
        let tunnel = Tunnel::open(&ssh, &target, &self.private("tunnels")?, &r.socket)?;
        let token = self
            .setup
            .secrets
            .get(&secret_name(&r.name))?
            .ok_or_else(|| anyhow::anyhow!("no token kept for {}", r.name))?;
        self.links.add(Link::new(
            r.name.clone(),
            format!("unix:{}", tunnel.socket().display()),
            token,
        ));
        self.tunnels.lock().unwrap().insert(r.name.clone(), tunnel);
        Ok(())
    }

    /// Set up `target` and add it. Progress goes to `emit` as `{kind: "computer_setup",
    /// target, step}`, then `{.., done: true, name}` or `{.., failure: {step, words, terminal}}`.
    pub(crate) fn add(
        self: &Arc<Self>,
        target: &str,
        token_name: String,
        emit: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> anyhow::Result<()> {
        let target = Target::parse(target.trim()).map_err(anyhow::Error::msg)?;
        let key = target.to_string();
        if !self.busy.lock().unwrap().insert(key.clone()) {
            anyhow::bail!("{} is being set up", target.host());
        }
        let me = self.clone();
        tokio::spawn(async move {
            let out = me.run_setup(&target, &token_name, &emit).await;
            me.busy.lock().unwrap().remove(&key);
            match out {
                Ok(name) => emit(
                    json!({ "kind": "computer_setup", "target": key, "done": true, "name": name }),
                ),
                Err((step, words, terminal)) => {
                    if let Some(t) = &terminal {
                        me.terminals.lock().unwrap().insert(key.clone(), t.clone());
                    }
                    emit(json!({
                        "kind": "computer_setup", "target": key,
                        "failure": { "step": step, "words": words, "terminal": terminal.is_some() },
                    }))
                }
            }
        });
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    async fn run_setup(
        &self,
        target: &Target,
        token_name: &str,
        emit: &Arc<dyn Fn(Value) + Send + Sync>,
    ) -> Result<String, (String, String, Option<Vec<String>>)> {
        let fail = |step: &str, words: String| (step.to_string(), words, None);
        let ssh = Ssh::system(self.setup.ssh_config.clone())
            .ok_or_else(|| fail("connect", "This computer has no ssh".into()))?;
        let dir = self
            .private("ssh")
            .map_err(|e| fail("connect", e.to_string()))?;
        let session = Session::new(ssh.clone(), target.clone(), &dir)
            .map_err(|e| fail("connect", e.to_string()))?;
        let failure = |f: ark_reach::Failure| {
            let step = serde_json::to_value(f.step)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let mut words = f.words;
            if let Some(last) = f.log.last() {
                words = format!("{words}: {last}");
            }
            (step, words, f.terminal)
        };
        let key = target.to_string();
        let step = |s: ark_reach::Step| {
            emit(json!({ "kind": "computer_setup", "target": key, "step": s }));
        };
        // The build depends on what is there: look first, then set up with it.
        step(ark_reach::Step::Connect);
        session.connect().await.map_err(failure)?;
        step(ark_reach::Step::Look);
        let facts = session
            .look(self.setup.contract.as_ref())
            .await
            .map_err(failure)?;
        let build = (self.setup.build)(&facts).map_err(|e| fail("install", format!("{e:#}")))?;
        let done = ark_reach::setup(
            &session,
            self.setup.contract.as_ref(),
            Plan {
                build: &build,
                token: Some(token_name),
            },
            step,
        )
        .await
        .map_err(failure)?;
        session.close().await;
        let name = done.host.name.clone();
        let name = if name.is_empty() {
            target.host().to_string()
        } else {
            name
        };
        let token = done
            .token
            .ok_or_else(|| fail("pair", "No token came back".into()))?;
        self.setup
            .secrets
            .set(&secret_name(&name), token.expose())
            .map_err(|e| fail("pair", e.to_string()))?;
        let r = Record {
            name: name.clone(),
            target: target.to_string(),
            socket: done.started.socket.clone(),
            linger: done.started.linger,
            token: token_name.to_string(),
            url: None,
        };
        if let Some(t) = done.started.linger_terminal {
            self.terminals.lock().unwrap().insert(r.target.clone(), t);
        }
        let _ = self.engine.store.put_record(
            KIND,
            &name,
            "",
            &serde_json::to_value(&r).unwrap_or_default(),
        );
        emit(json!({ "kind": "computer_setup", "target": r.target, "step": "tunnel" }));
        self.connect(&r)
            .map_err(|e| fail("tunnel", format!("{e:#}")))?;
        let up = {
            let t = self.tunnels.lock().unwrap();
            t.get(&name).map(|t| t.state())
        };
        if let Some(mut st) = up {
            let ok = tokio::time::timeout(
                Duration::from_secs(20),
                st.wait_for(|s| !matches!(s, ark_reach::TunnelState::Connecting)),
            )
            .await;
            if let Ok(Ok(s)) = ok {
                if let ark_reach::TunnelState::Down(why) = &*s {
                    return Err(fail("tunnel", why.clone()));
                }
            }
        }
        Ok(name)
    }

    /// Add the computer at `url` (an ARK runtime with a public address, ADR 0047) with a code
    /// made there: the code becomes a session, kept as this computer's token for it.
    pub(crate) async fn add_address(&self, url: &str, code: &str) -> anyhow::Result<String> {
        let (url, session) = redeem(url, code).await?;
        let name = reqwest::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| url.clone());
        self.keep_address(&name, &url, &session)?;
        Ok(name)
    }

    /// Keep a computer reached at `url` with `token` (an app moving what it kept before).
    pub(crate) fn keep_address(&self, name: &str, url: &str, token: &str) -> anyhow::Result<()> {
        self.setup.secrets.set(&secret_name(name), token)?;
        let r = Record {
            name: name.to_string(),
            target: url.to_string(),
            linger: true,
            url: Some(url.to_string()),
            ..Default::default()
        };
        self.engine
            .store
            .put_record(KIND, name, "", &serde_json::to_value(&r)?)?;
        let _ = self.engine.store.delete_record(REMOVED, name);
        self.connect(&r)
    }

    /// The command a step left for the person (`ssh` asking about a host key, `sudo` to keep
    /// the service running after logout), for a terminal on this computer.
    pub(crate) fn terminal_for(&self, target: &str) -> Option<Vec<String>> {
        self.terminals.lock().unwrap().get(target).cloned()
    }

    /// Take ARK off `name`: forget there the keys this computer sent, revoke its token, stop
    /// and delete the program (and its store there when `delete_store`); then forget it here:
    /// its token, its link, its chats. `here_only`: a computer that no longer answers (gone,
    /// rebuilt) is forgotten here alone; what was there stays there.
    pub(crate) async fn remove(
        &self,
        name: &str,
        delete_store: bool,
        here_only: bool,
    ) -> anyhow::Result<()> {
        let r = self
            .record(name)
            .ok_or_else(|| anyhow::anyhow!("{name} is not added here"))?;
        if !here_only {
            // The keys this computer sent there go first, while it still answers.
            let sent = ark_interact::held::keys_sent(&self.engine, name);
            if !sent.is_empty() {
                anyhow::ensure!(
                    self.links.state(name) == Some(ark_interact::links::State::Ok),
                    "{name} is not answering, so the keys this computer sent there cannot be forgotten there. Remove it here only, and they stay on {name}"
                );
                self.links
                    .chat(name, "chat_key", json!({ "forget": sent }))
                    .await?;
            }
            if r.url.is_none() {
                let ssh = Ssh::system(self.setup.ssh_config.clone())
                    .ok_or_else(|| anyhow::anyhow!("this computer has no ssh"))?;
                let target = Target::parse(&r.target).map_err(anyhow::Error::msg)?;
                let session = Session::new(ssh, target, &self.private("ssh")?)?;
                let done = ark_reach::remove(
                    &session,
                    self.setup.contract.as_ref(),
                    Some(&r.token),
                    delete_store,
                )
                .await;
                session.close().await;
                done.map_err(|f| anyhow::anyhow!(f.words))?;
            }
        }
        ark_interact::held::forget_keys_sent(&self.engine, name);
        let _ = self
            .engine
            .store
            .put_record(REMOVED, name, "", &json!({ "name": name }));
        self.tunnels.lock().unwrap().remove(name);
        self.links.remove(name);
        let _ = self.setup.secrets.delete(&secret_name(name));
        self.engine.store.delete_record(KIND, name)?;
        // Its chats were there: they leave this computer's lists too.
        for c in ark_interact::held::list(&self.engine, None) {
            if c.place == name {
                let _ = ark_interact::held::forget(&self.engine, &c.session);
            }
        }
        Ok(())
    }
}

/// Trade a code for a session at the runtime at `url`: the address as kept, and the session.
async fn redeem(url: &str, code: &str) -> anyhow::Result<(String, String)> {
    let url = url.trim().trim_end_matches('/').to_string();
    let parsed = reqwest::Url::parse(&url)
        .map_err(|_| anyhow::anyhow!("Type the whole address, starting with https://"))?;
    let local = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    anyhow::ensure!(
        parsed.scheme() == "https" || (local && parsed.scheme() == "http"),
        "The address must start with https:// (the code and the session travel over it)"
    );
    let res = reqwest::Client::new()
        .post(format!("{url}/session"))
        .header("Origin", parsed.origin().ascii_serialization())
        .json(&json!({ "code": code.trim(), "name": "ARK" }))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| {
            anyhow::anyhow!("{url} is not answering. Check the address and that ARK runs there.")
        })?;
    anyhow::ensure!(
        res.status().is_success(),
        "That code did not work. Make a new one there: Settings › Devices › Add a device."
    );
    let session = res
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| {
            let (_, v) = c.split(';').next()?.split_once('=')?;
            v.starts_with("arks_").then(|| v.to_string())
        })
        .ok_or_else(|| anyhow::anyhow!("{url} did not sign this computer in. Is ARK there?"))?;
    Ok((url, session))
}
