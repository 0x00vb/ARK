//! When the runtime notifies a person's devices (ADR 0047), and what it sends. Something that
//! waits (the attention list, a chat's own question) is sent once, a minute after it appeared
//! and only if it still waits, to every device added before it whose page is not open and in
//! view. A device a revoked token or session added is forgotten.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use ark_interact::attention::Notice;
use ark_sdk::prelude::Engine;
use serde_json::{json, Value};

use crate::access::Tokens;
use crate::push::{self, Device, Keys, Message, Sent};

/// How long something waits before a device hears of it: answered at the desk, it never buzzes
/// a phone. `ARK_PUSH_WAIT_MS` sets it (tests).
fn wait() -> Duration {
    std::env::var("ARK_PUSH_WAIT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(60))
}

/// How long a page's "I am in view" lasts without another.
const IN_VIEW_MS: u64 = 90_000;

pub(crate) struct Notifier {
    engine: Arc<Engine>,
    keys: OnceLock<Option<Keys>>,
    client: OnceLock<reqwest::Client>,
    scheduled: AtomicBool,
    /// Device id -> until when its page said it was in view.
    in_view: Mutex<HashMap<String, u64>>,
    /// The runtime's tokens, when it serves with saved tokens.
    tokens: OnceLock<Tokens>,
    /// Sends one message (the push service; a fake in tests).
    out: OnceLock<Out>,
}

type Out = Arc<
    dyn Fn(Device, Message) -> std::pin::Pin<Box<dyn std::future::Future<Output = Sent> + Send>>
        + Send
        + Sync,
>;

impl Notifier {
    pub(crate) fn new(engine: Arc<Engine>) -> Notifier {
        Notifier {
            engine,
            keys: OnceLock::new(),
            client: OnceLock::new(),
            scheduled: AtomicBool::new(false),
            in_view: Mutex::new(HashMap::new()),
            tokens: OnceLock::new(),
            out: OnceLock::new(),
        }
    }

    /// The runtime serves with saved tokens: a device lasts as long as what added it.
    pub(crate) fn use_tokens(&self, tokens: Tokens) {
        let _ = self.tokens.set(tokens);
    }

    /// Send through `out` instead of the push services (tests).
    #[cfg(test)]
    pub(crate) fn send_with(&self, out: Out) {
        let _ = self.out.set(out);
    }

    fn keys(&self) -> Option<&Keys> {
        self.keys
            .get_or_init(|| {
                Keys::in_file(&push::key_file(self.engine.store.root()))
                    .map_err(|e| tracing::warn!("no push key: {e:#}"))
                    .ok()
            })
            .as_ref()
    }

    /// The runtime's public key, for a page to subscribe with.
    pub(crate) fn public_key(&self) -> Option<String> {
        self.keys().map(Keys::public)
    }

    pub(crate) fn devices(&self) -> Vec<Device> {
        self.engine
            .store
            .list_records(push::DEVICE, None)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect()
    }

    /// Add (or renew) a device. `sub` is the browser's subscription (`{endpoint, keys: {p256dh,
    /// auth}}`).
    pub(crate) fn add(
        &self,
        sub: &Value,
        name: &str,
        by: &str,
        origin: &str,
    ) -> anyhow::Result<()> {
        let endpoint = sub["endpoint"].as_str().unwrap_or("");
        anyhow::ensure!(
            push::known_service(endpoint),
            "this browser's push service is not one ARK sends to"
        );
        let s = |k: &str| sub["keys"][k].as_str().unwrap_or("").to_string();
        let (p256dh, auth) = (s("p256dh"), s("auth"));
        anyhow::ensure!(
            !p256dh.is_empty() && !auth.is_empty(),
            "the subscription has no keys"
        );
        let id = Device::id(endpoint);
        let known = self.devices();
        anyhow::ensure!(
            known.iter().any(|d| Device::id(&d.endpoint) == id) || known.len() < push::MAX_DEVICES,
            "{} devices get notifications already; turn one off first",
            push::MAX_DEVICES
        );
        let d = Device {
            endpoint: endpoint.into(),
            p256dh,
            auth,
            name: name.trim().chars().take(60).collect(),
            added_ms: now_ms(),
            by: by.into(),
            origin: origin.into(),
        };
        self.engine
            .store
            .put_record(push::DEVICE, &id, "", &serde_json::to_value(&d)?)?;
        Ok(())
    }

    pub(crate) fn remove(&self, endpoint: &str) -> bool {
        self.engine
            .store
            .delete_record(push::DEVICE, &Device::id(endpoint))
            .unwrap_or(false)
    }

    pub(crate) fn remove_id(&self, id: &str) -> bool {
        self.engine
            .store
            .delete_record(push::DEVICE, id)
            .unwrap_or(false)
    }

    /// The page of the device `endpoint` is open and in view (or not any more).
    pub(crate) fn in_view(&self, endpoint: &str, yes: bool) {
        let mut v = self.in_view.lock().unwrap();
        if yes {
            v.insert(Device::id(endpoint), now_ms() + IN_VIEW_MS);
        } else {
            v.remove(&Device::id(endpoint));
        }
    }

    /// Something may wait now: look again once it has waited long enough. One look is pending
    /// at a time.
    pub(crate) fn soon(self: &Arc<Self>, notices: Arc<dyn Fn() -> Vec<Notice> + Send + Sync>) {
        if self.devices().is_empty() || self.scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(wait()).await;
                let pending = me.look(&notices()).await;
                if !pending {
                    break;
                }
            }
            me.scheduled.store(false, Ordering::SeqCst);
        });
    }

    /// Send what is due; true when something waits that is not due yet.
    async fn look(&self, notices: &[Notice]) -> bool {
        let live = self
            .tokens
            .get()
            .and_then(|t| t.list().ok())
            .map(|l| l.into_iter().map(|t| t.name).collect::<HashSet<String>>());
        let mut devices = Vec::new();
        for d in self.devices() {
            // What added it was revoked or ran out.
            if live.as_ref().is_some_and(|names| !names.contains(&d.by)) {
                self.remove(&d.endpoint);
                continue;
            }
            devices.push(d);
        }
        let in_view = self.in_view.lock().unwrap().clone();
        let done = ark_interact::attention::notified(&self.engine);
        let (sends, due, pending) = plan(
            &devices,
            notices,
            &done,
            &in_view,
            now_ms(),
            wait().as_millis() as u64,
        );
        for (di, ni) in sends {
            let (d, n) = (&devices[di], &notices[ni]);
            let m = Message {
                id: n.id.clone(),
                title: n.title.clone(),
                body: n.body.clone(),
                tag: n.tag.clone(),
                urgent: n.urgent,
            };
            match self.send(d.clone(), m).await {
                Sent::Ok => {}
                Sent::Gone => {
                    self.remove(&d.endpoint);
                }
                other => tracing::info!(device = %d.name, "a notification was not sent: {other:?}"),
            }
        }
        for ni in due {
            let _ = ark_interact::attention::mark_notified(&self.engine, &notices[ni].id);
        }
        pending
    }

    async fn send(&self, d: Device, m: Message) -> Sent {
        if let Some(out) = self.out.get() {
            return out(d, m).await;
        }
        let Some(keys) = self.keys() else {
            return Sent::Refused("no push key".into());
        };
        let client = self
            .client
            .get_or_init(ark_sdk::prelude::ark_tools::web::public_client);
        push::send(client, keys, &d, &m).await
    }

    /// What `push_devices` answers: names and times, never keys.
    pub(crate) fn listed(&self) -> Value {
        json!(self
            .devices()
            .iter()
            .map(
                |d| json!({ "name": d.name, "added_ms": d.added_ms, "id": Device::id(&d.endpoint) })
            )
            .collect::<Vec<_>>())
    }
}

/// Which device hears of which notice now (indexes), which notices are done (sent, or no device
/// should hear of them), and whether something waits that is not due yet.
fn plan(
    devices: &[Device],
    notices: &[Notice],
    done: &HashSet<String>,
    in_view: &HashMap<String, u64>,
    now: u64,
    wait: u64,
) -> (Vec<(usize, usize)>, Vec<usize>, bool) {
    let (mut sends, mut due, mut pending) = (Vec::new(), Vec::new(), false);
    for (ni, n) in notices.iter().enumerate() {
        if done.contains(&n.id) {
            continue;
        }
        if now.saturating_sub(n.at_ms) < wait {
            pending = true;
            continue;
        }
        due.push(ni);
        for (di, d) in devices.iter().enumerate() {
            let looking = in_view
                .get(&Device::id(&d.endpoint))
                .is_some_and(|until| *until > now);
            // Nothing from before the device asked; nothing to a page that shows it already.
            if n.at_ms >= d.added_ms && !looking {
                sends.push((di, ni));
            }
        }
    }
    (sends, due, pending)
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(endpoint: &str, added_ms: u64) -> Device {
        Device {
            endpoint: endpoint.into(),
            p256dh: String::new(),
            auth: String::new(),
            name: "phone".into(),
            added_ms,
            by: "phone".into(),
            origin: String::new(),
        }
    }

    fn notice(id: &str, at_ms: u64) -> Notice {
        Notice {
            id: id.into(),
            title: "p".into(),
            body: "b".into(),
            tag: id.into(),
            urgent: false,
            at_ms,
        }
    }

    #[test]
    fn once_after_the_wait_to_devices_that_asked_before_and_are_not_looking() {
        let devices = [
            device("https://a", 100),
            device("https://b", 100),
            device("https://c", 5_000),
        ];
        let notices = [
            notice("old", 50),
            notice("due", 1_000),
            notice("fresh", 9_500),
            notice("sent", 1_000),
        ];
        let done: HashSet<String> = ["sent".to_string()].into();
        let in_view: HashMap<String, u64> = [(Device::id("https://b"), 20_000)].into();
        let (sends, due, pending) = plan(&devices, &notices, &done, &in_view, 10_000, 1_000);
        // "old" is older than every device: done, sent to nobody. "due" goes to a only: b is
        // looking, c asked later. "fresh" is not due. "sent" went out before.
        assert_eq!(sends, vec![(0, 1)]);
        assert_eq!(due, vec![0, 1]);
        assert!(pending);
    }
}

#[cfg(test)]
mod host_tests {
    use std::sync::Arc;

    use ark_interact::App;
    use ark_sdk::prelude::*;
    use serde_json::json;

    use crate::push::Sent;

    /// A failed task in some project: one notification per device, with the list's words, once
    /// it waited long enough, and not again. A revoked device hears nothing.
    #[tokio::test]
    async fn something_that_waits_reaches_each_device_once() {
        std::env::set_var("ARK_PUSH_WAIT_MS", "50");
        let dir = tempfile::tempdir().unwrap();
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(Arc::new(ark_model::MockProvider::new("m", vec![])))
            .build()
            .unwrap();
        let (host, _) = crate::ArkUi::new(App::new("Test").ark(ark))
            .open()
            .await
            .unwrap();
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let s = sent.clone();
        host.notifier.send_with(Arc::new(move |d, m| {
            s.lock().unwrap().push((
                d.name.clone(),
                m.title.clone(),
                m.body.clone(),
                m.tag.clone(),
            ));
            Box::pin(async { Sent::Ok })
        }));
        let sub = |n: &str| json!({ "endpoint": format!("https://fcm.googleapis.com/fcm/send/{n}"), "keys": { "p256dh": "BCVx", "auth": "BTBZ" } });
        host.call(
            "push_add",
            json!({ "subscription": sub("a"), "name": "phone" }),
        )
        .await
        .unwrap();
        // Not a push service ARK sends to.
        assert!(host
            .call("push_add", json!({ "subscription": { "endpoint": "https://example.com/x", "keys": { "p256dh": "a", "auth": "b" } }, "name": "x" }))
            .await
            .is_err());
        let listed = host.call("push_devices", json!({})).await.unwrap();
        assert_eq!(listed[0]["name"], "phone");
        assert!(
            listed[0].get("p256dh").is_none(),
            "names and times, never keys"
        );

        let project = host.call("summary", json!({})).await.unwrap()["project"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let now = super::now_ms();
        host.engine_for_tests()
            .store
            .put_record(
                "orch_task",
                "t1",
                &project,
                &json!({ "id": "t1", "project": project, "owner": "helper", "objective": "file the notes",
                    "origin": { "by": "external" }, "state": "failed", "created_ms": now, "finished_ms": now + 1,
                    "error": "it broke" }),
            )
            .unwrap();
        host.notify_soon();
        for _ in 0..100 {
            if !sent.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        host.notify_soon();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let sent = sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "{sent:?}");
        let (device, title, body, tag) = &sent[0];
        assert_eq!(device, "phone");
        assert!(title.ends_with("· helper"), "{title}");
        assert!(body.contains("file the notes"), "{body}");
        assert_eq!(tag, &format!("work/{project}"));
    }
}
