//! Other runtimes whose waiting items this one shows too (ADR 0043 step 4): a person's own
//! server beside this computer. Each link is followed here and its list kept in memory; nothing
//! is copied into this runtime's store, and each item is answered on its own runtime.
//!
//! ```text
//! app ── set([My machine: url, token]) ─► Links ── follow: B's `attention`, and on each of its
//!                                                   `attention` events, B's list again
//! window, /waiting ── everywhere() ──► this runtime's list + each link's (named, kept)
//! answer, clear, open ──────────────► that runtime: only these, never any other call
//! ```
//!
//! Who may set links: the app, in Rust (`ArkUi::link`, `Links::set`). A link's token stays in
//! this process: it is never in an answer, an event or a log. A client of this runtime reaches
//! a linked one only through the four calls below, as this runtime's owner.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::attention::Item;
use crate::remote::Client;
use ark_sdk::prelude::ApprovalDecision;

/// A runtime elsewhere: what a person calls it, where it is, and the token this runtime uses
/// there.
#[derive(Clone)]
pub struct Link {
    pub name: String,
    pub url: String,
    token: String,
}

impl Link {
    pub fn new(name: impl Into<String>, url: impl Into<String>, token: impl Into<String>) -> Link {
        Link {
            name: name.into(),
            url: url.into(),
            token: token.into(),
        }
    }
}

impl std::fmt::Debug for Link {
    /// Never the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("name", &self.name)
            .field("url", &self.url)
            .finish()
    }
}

/// How a link is, as a client shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Not read yet.
    Starting,
    Ok,
    /// Not answering (down, asleep, the network).
    Down,
    /// It refused this runtime's token: pair again.
    Refused,
    /// It answers, but has no list to give (an older version).
    TooOld,
}

/// A link as a client sees it: its name, its state, and whether it had items when it last
/// answered (a link that had some and stopped answering says so).
#[derive(Clone, Debug, Serialize)]
pub struct Shown {
    pub name: String,
    pub state: State,
    pub had_items: bool,
}

struct Followed {
    link: Link,
    client: Client,
    state: Mutex<State>,
    items: Mutex<Vec<Item>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// The links of one app, shared by its conversations.
pub struct Links {
    followed: Mutex<HashMap<String, Arc<Followed>>>,
    changed: broadcast::Sender<()>,
}

impl Default for Links {
    fn default() -> Self {
        Links {
            followed: Mutex::new(HashMap::new()),
            changed: broadcast::channel(16).0,
        }
    }
}

impl Drop for Links {
    /// The followers end with the app.
    fn drop(&mut self) {
        for f in self.followed.lock().unwrap().values() {
            if let Some(t) = f.task.lock().unwrap().take() {
                t.abort();
            }
        }
    }
}

/// How long a link's stream may be silent (it pings every 15 s) before it counts as gone.
const IDLE: Duration = Duration::from_secs(40);
/// How long one call to a link may take.
const CALL: Duration = Duration::from_secs(8);

impl Links {
    /// From now on, these links (replacing the ones before). Each is followed in the
    /// background; call within a tokio runtime.
    pub fn set(self: &Arc<Self>, links: Vec<Link>) {
        // A link without a token could only be refused.
        let links: Vec<Link> = links.into_iter().filter(|l| !l.token.is_empty()).collect();
        let mut followed = self.followed.lock().unwrap();
        // Gone or changed: stop following.
        followed.retain(|name, f| {
            let keep = links
                .iter()
                .any(|l| l.name == *name && l.url == f.link.url && l.token == f.link.token);
            if !keep {
                if let Some(t) = f.task.lock().unwrap().take() {
                    t.abort();
                }
            }
            keep
        });
        for l in links {
            if followed.contains_key(&l.name) {
                continue;
            }
            let f = Arc::new(Followed {
                client: Client::new(&l.url, &l.token),
                link: l.clone(),
                state: Mutex::new(State::Starting),
                items: Mutex::new(Vec::new()),
                task: Mutex::new(None),
            });
            let task = tokio::spawn(follow(f.clone(), self.changed.clone()));
            *f.task.lock().unwrap() = Some(task);
            followed.insert(l.name.clone(), f);
        }
        let _ = self.changed.send(());
    }

    /// Something changed on a link (its list, its state).
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.changed.subscribe()
    }

    pub fn is_empty(&self) -> bool {
        self.followed.lock().unwrap().is_empty()
    }

    /// Every link's items as last read, named: their `runtime` is the link's name, their place
    /// starts with it. Never waits on the network.
    pub fn items(&self) -> Vec<Item> {
        let followed = self.followed.lock().unwrap();
        let mut names: Vec<&String> = followed.keys().collect();
        names.sort();
        names
            .into_iter()
            .flat_map(|n| followed[n].items.lock().unwrap().clone())
            .collect()
    }

    /// Every link's state.
    pub fn shown(&self) -> Vec<Shown> {
        let followed = self.followed.lock().unwrap();
        let mut out: Vec<Shown> = followed
            .values()
            .map(|f| Shown {
                name: f.link.name.clone(),
                state: *f.state.lock().unwrap(),
                had_items: !f.items.lock().unwrap().is_empty(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn get(&self, runtime: &str) -> anyhow::Result<Arc<Followed>> {
        self.followed
            .lock()
            .unwrap()
            .get(runtime)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{runtime} is not linked here"))
    }

    /// Answer a question waiting on `runtime` (its kept question `id`).
    pub async fn answer(&self, runtime: &str, id: &str, d: ApprovalDecision) -> anyhow::Result<()> {
        let f = self.get(runtime)?;
        call(&f, "approve", json!({ "id": id, "decision": d })).await?;
        Ok(())
    }

    /// Clear a result or a failure on `runtime`.
    pub async fn clear(&self, runtime: &str, id: &str) -> anyhow::Result<()> {
        let f = self.get(runtime)?;
        call(&f, "attention_clear", json!({ "id": id })).await?;
        Ok(())
    }

    /// Open an item on `runtime`: it is cleared there when it is a result or a failure, and its
    /// text comes back (`attention_open` there).
    pub async fn open(&self, runtime: &str, id: &str) -> anyhow::Result<Value> {
        let f = self.get(runtime)?;
        call(&f, "attention_open", json!({ "id": id })).await
    }

    /// The words for an error that came from reaching `runtime`, without its address or token.
    pub fn words(&self, runtime: &str, e: &anyhow::Error) -> String {
        match self.get(runtime) {
            Ok(f) => plain(e, &f.link),
            Err(_) => format!("{runtime} is not linked here"),
        }
    }

    /// Where `runtime` is and the token for it, for a window to show its page. Rust only.
    pub fn page(&self, runtime: &str) -> Option<(String, String)> {
        let f = self.get(runtime).ok()?;
        Some((f.link.url.clone(), f.link.token.clone()))
    }
}

async fn call(f: &Followed, method: &str, params: Value) -> anyhow::Result<Value> {
    let name = &f.link.name;
    match tokio::time::timeout(CALL, f.client.call(method, params)).await {
        Ok(Ok(v)) => Ok(v),
        // In words, without the address or the token.
        Ok(Err(e)) => match classify(&e) {
            State::Ok => Err(anyhow::anyhow!(
                "{name}: {}",
                strip(&format!("{e:#}"), &f.link)
            )),
            _ => Err(anyhow::anyhow!(plain(&e, &f.link))),
        },
        Err(_) => anyhow::bail!("{name} is not answering"),
    }
}

/// An error's words without the link's address or token.
fn strip(s: &str, l: &Link) -> String {
    s.replace(&l.token, "…")
        .replace(&l.url, &l.name)
        .replace(l.url.trim_end_matches('/'), &l.name)
}

/// The words for an error from `runtime`, without its address or token.
pub fn plain(e: &anyhow::Error, l: &Link) -> String {
    match classify(e) {
        State::Refused => format!("{} signed this computer out: pair again", l.name),
        State::TooOld => format!("{} runs an older version that cannot do this", l.name),
        State::Down => format!("{} is not answering", l.name),
        _ => strip(&format!("{e:#}"), l),
    }
}

/// What a failed read says about a link: an answer it should not give counts as not
/// answering.
fn trouble(e: &anyhow::Error) -> State {
    match classify(e) {
        State::Ok => State::Down,
        s => s,
    }
}

/// What an error from a link says about it: the client's own words for a 401, the runtime's
/// for a method it does not have, the client's for no answer. Anything else is an answer (the
/// runtime is there and said no): `Ok`.
fn classify(e: &anyhow::Error) -> State {
    let s = e.to_string();
    if s.starts_with("the ARK runtime at ") && s.ends_with(" refused the access token") {
        State::Refused
    } else if s.starts_with("unknown method `") {
        State::TooOld
    } else if s.starts_with("cannot reach the ARK runtime at ")
        || (s.starts_with("the ARK runtime at ")
            && (s.contains(" answered HTTP ") || s.ends_with(" closed the event stream")))
        || s == "timed out"
    {
        State::Down
    } else {
        State::Ok
    }
}

/// Keep `f`'s list: read it, then read it again on each of its `attention` events; when the
/// stream goes, back off and start over.
async fn follow(f: Arc<Followed>, changed: broadcast::Sender<()>) {
    let set = |state: State| {
        let before = std::mem::replace(&mut *f.state.lock().unwrap(), state);
        if before != state {
            let _ = changed.send(());
        }
    };
    // Tell clients only when the list differs: two runtimes that link each other would
    // otherwise read each other for ever.
    let read = || async {
        let v = tokio::time::timeout(CALL, f.client.call("attention", json!({})))
            .await
            .map_err(|_| anyhow::anyhow!("timed out"))??;
        // One by one: an item a newer runtime words differently is left out, not the list.
        let items: Vec<Item> = v
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| serde_json::from_value::<Item>(v.clone()).ok())
            .map(|mut it| {
                it.runtime = Some(f.link.name.clone());
                it.place = format!("{} · {}", f.link.name, it.place);
                it
            })
            .collect();
        let differs = {
            let mut kept = f.items.lock().unwrap();
            let differs = serde_json::to_value(&*kept).ok() != serde_json::to_value(&items).ok();
            *kept = items;
            differs
        };
        if differs {
            let _ = changed.send(());
        }
        anyhow::Ok(())
    };
    let mut backoff = Duration::from_secs(1);
    let wait = |backoff: &mut Duration| {
        let now = *backoff;
        *backoff = (*backoff * 2).min(Duration::from_secs(60));
        tokio::time::sleep(now)
    };
    loop {
        if let Err(e) = read().await {
            set(trouble(&e));
            wait(&mut backoff).await;
            continue;
        }
        set(State::Ok);
        let mut rx = match f.client.events_alive().await {
            Ok(rx) => rx,
            Err(e) => {
                set(trouble(&e));
                wait(&mut backoff).await;
                continue;
            }
        };
        let mut heard = false;
        let ended = loop {
            match tokio::time::timeout(IDLE, rx.recv()).await {
                // What a linked runtime emits because of its own links is not news here.
                Ok(Some(ev)) if ev["kind"] == "attention" && ev["linked"] != true => {
                    heard = true;
                    if let Err(e) = read().await {
                        break Some(e);
                    }
                }
                Ok(Some(_)) => heard = true,
                // The stream ended, or went silent: start over.
                Ok(None) | Err(_) => break None,
            }
        };
        set(ended.as_ref().map(trouble).unwrap_or(State::Down));
        // A stream that ended before a word is no better than no stream: back off.
        if heard {
            backoff = Duration::from_secs(1);
        } else {
            wait(&mut backoff).await;
        }
    }
}
