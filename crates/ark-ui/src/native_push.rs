//! Optional, content-free notification relay for authenticated external clients.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use ark_interact::attention::Notice;
use ark_sdk::prelude::{ark_connect::Secrets, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::access::Tokens;

const REGISTRATIONS: &str = "native_push_registration";
const DELIVERIES: &str = "native_push_delivery";
const COMPLETIONS: &str = "native_push_completion";
const MAX_AGE_MS: u64 = 3_600_000;

#[derive(Clone)]
struct Config {
    origin: String,
    route: String,
    until: u64,
}

impl Config {
    fn environment() -> Option<Self> {
        Self::parse(
            &std::env::var("ARK_NATIVE_PUSH_RELAY_ORIGIN").ok()?,
            &std::env::var("ARK_NATIVE_PUSH_ROUTE_ID").ok()?,
            &std::env::var("ARK_NATIVE_PUSH_UNTIL").ok()?,
        )
        .ok()
    }

    fn parse(origin: &str, route: &str, until: &str) -> anyhow::Result<Self> {
        let url = reqwest::Url::parse(origin)?;
        let host = url.host_str().unwrap_or("");
        anyhow::ensure!(
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.port_or_known_default() == Some(443)
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
                && host.contains('.')
                && !host.ends_with(".local")
                && !host.ends_with(".localhost")
                && host.parse::<std::net::IpAddr>().is_err(),
            "invalid notification relay configuration"
        );
        anyhow::ensure!(opaque(route, 1, 200), "invalid notification route");
        let until = timestamp(until)?;
        anyhow::ensure!(
            until > now_seconds() && until <= now_seconds() + 31 * 86_400,
            "notification pilot expiry is out of range"
        );
        Ok(Self {
            origin: url.origin().ascii_serialization(),
            route: route.into(),
            until,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Registration {
    id: String,
    owner: String,
    route: String,
    expires_at: String,
    expires: u64,
    added_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct DeliveryBody {
    request_id: String,
    attention_id: String,
    collapse_id: String,
    expiry: u64,
    kind: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Delivery {
    registration: String,
    owner: String,
    notice_id: String,
    chat_id: Option<String>,
    body: DeliveryBody,
    state: String,
    attempts: u8,
    next_attempt: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    receipt_id: String,
    state: String,
}

#[derive(Serialize, Deserialize)]
struct Completion {
    id: String,
    chat: String,
    at_ms: u64,
    kind: String,
}

#[cfg(test)]
type TestOut = Arc<dyn Fn(&DeliveryBody) -> Receipt + Send + Sync>;

pub(crate) struct NativePush {
    engine: Arc<Engine>,
    config: OnceLock<Config>,
    client: reqwest::Client,
    presence: Mutex<HashMap<String, u64>>,
    changes: Mutex<()>,
    secrets: Option<Arc<dyn Secrets>>,
    #[cfg(test)]
    out: std::sync::OnceLock<TestOut>,
}

impl NativePush {
    pub(crate) fn new(engine: Arc<Engine>, secrets: Option<Arc<dyn Secrets>>) -> Self {
        Self {
            engine,
            config: Config::environment()
                .map(OnceLock::from)
                .unwrap_or_default(),
            client: ark_sdk::prelude::ark_tools::web::public_client_no_redirect(),
            presence: Mutex::new(HashMap::new()),
            changes: Mutex::new(()),
            secrets,
            #[cfg(test)]
            out: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn enabled(&self, route: Option<&str>) -> bool {
        self.secrets.is_some()
            && self
                .config
                .get()
                .is_some_and(|c| c.until > now_seconds() && Some(c.route.as_str()) == route)
    }

    fn secrets(&self) -> anyhow::Result<&dyn Secrets> {
        self.secrets
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("notification secret storage unavailable"))
    }

    pub(crate) fn register(
        &self,
        owner: &str,
        route: &str,
        request: ark_protocol::NativePushRegistrationRequest,
    ) -> anyhow::Result<ark_protocol::NativePushRegistrationResponse> {
        let _guard = self.changes.lock().unwrap();
        let c = self
            .config
            .get()
            .filter(|_| self.enabled(Some(route)))
            .ok_or_else(|| anyhow::anyhow!("notifications are not enabled here"))?;
        anyhow::ensure!(
            request.runtime_route_id == c.route,
            "notification route does not match"
        );
        anyhow::ensure!(
            uuid::Uuid::parse_str(&request.registration_id).is_ok(),
            "invalid notification registration"
        );
        anyhow::ensure!(
            opaque(&request.send_capability, 43, 43),
            "invalid notification capability"
        );
        let expires = timestamp(&request.expires_at)?;
        anyhow::ensure!(
            expires > now_seconds() && expires <= c.until,
            "invalid notification expiry"
        );
        let old = self.registration(&request.registration_id)?;
        anyhow::ensure!(
            old.as_ref()
                .is_none_or(|r| r.owner == owner && r.route == route),
            "notification registration belongs to another device"
        );
        let registrations = self.engine.store.list_records(REGISTRATIONS, None)?;
        anyhow::ensure!(
            old.is_some() || registrations.len() < 10,
            "too many notification devices"
        );
        let registration = Registration {
            id: request.registration_id,
            owner: owner.into(),
            route: route.into(),
            expires_at: request.expires_at,
            expires,
            added_ms: old
                .map(|r| r.added_ms)
                .unwrap_or_else(crate::notify::now_ms),
        };
        self.secrets()?
            .set(&secret_key(&registration.id), &request.send_capability)
            .map_err(|_| anyhow::anyhow!("could not save notification capability"))?;
        self.engine.store.put_record(
            REGISTRATIONS,
            &registration.id,
            "",
            &serde_json::to_value(&registration)?,
        )?;
        Ok(ark_protocol::NativePushRegistrationResponse {
            registration_id: registration.id,
            runtime_route_id: registration.route,
            expires_at: registration.expires_at,
        })
    }

    fn registration(&self, id: &str) -> anyhow::Result<Option<Registration>> {
        self.engine
            .store
            .get_record(REGISTRATIONS, id)?
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }

    pub(crate) fn remove(&self, owner: &str, id: &str) -> anyhow::Result<()> {
        let _guard = self.changes.lock().unwrap();
        if let Some(registration) = self.registration(id)? {
            anyhow::ensure!(
                registration.owner == owner,
                "notification registration belongs to another device"
            );
            self.engine.store.delete_record(REGISTRATIONS, id)?;
            self.secrets()?
                .delete(&secret_key(id))
                .map_err(|_| anyhow::anyhow!("could not remove notification capability"))?;
            self.presence.lock().unwrap().remove(id);
        }
        Ok(())
    }

    pub(crate) fn present(
        &self,
        owner: &str,
        request: &ark_protocol::NativePushPresenceRequest,
    ) -> anyhow::Result<()> {
        let r = self
            .registration(&request.registration_id)?
            .filter(|r| r.owner == owner && r.expires > now_seconds())
            .ok_or_else(|| anyhow::anyhow!("notification registration is unavailable"))?;
        let mut presence = self.presence.lock().unwrap();
        if request.in_foreground {
            presence.insert(r.id, crate::notify::now_ms() + 45_000);
        } else {
            presence.remove(&r.id);
        }
        Ok(())
    }

    pub(crate) fn completed(
        &self,
        chat: &str,
        index: Option<u32>,
        successful: bool,
    ) -> anyhow::Result<()> {
        if !self.has_devices() {
            return Ok(());
        }
        let at_ms = crate::notify::now_ms();
        let id = digest(&[
            "completed",
            chat,
            &index
                .map(|i| i.to_string())
                .unwrap_or_else(|| at_ms.to_string()),
        ]);
        let value = Completion {
            id: id.clone(),
            chat: chat.into(),
            at_ms,
            kind: if successful {
                "completed"
            } else {
                "needs_attention"
            }
            .into(),
        };
        self.engine
            .store
            .put_record(COMPLETIONS, &id, "", &serde_json::to_value(value)?)?;
        Ok(())
    }

    pub(crate) fn resolve(
        &self,
        owner: &str,
        id: &str,
    ) -> anyhow::Result<Option<(String, Option<String>)>> {
        anyhow::ensure!(opaque(id, 64, 64), "invalid notification locator");
        for value in self.engine.store.list_records(DELIVERIES, None)? {
            let delivery: Delivery = serde_json::from_value(value)?;
            if delivery.owner == owner && delivery.body.attention_id == id {
                let live = self
                    .registration(&delivery.registration)?
                    .is_some_and(|r| r.owner == owner && r.expires > now_seconds());
                if live {
                    return Ok(Some((delivery.notice_id, delivery.chat_id)));
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn has_devices(&self) -> bool {
        self.config.get().is_some()
            && self
                .engine
                .store
                .list_records(REGISTRATIONS, None)
                .is_ok_and(|r| !r.is_empty())
    }

    pub(crate) async fn look(
        &self,
        notices: &[Notice],
        tokens: Option<&Tokens>,
        wait_ms: u64,
    ) -> anyhow::Result<bool> {
        let Some(c) = self.config.get().filter(|c| c.until > now_seconds()) else {
            return Ok(false);
        };
        let Some(tokens) = tokens else {
            return Ok(false);
        };
        let now = crate::notify::now_ms();
        let mut source: Vec<(String, Option<String>, u64, String)> = notices
            .iter()
            .map(|n| {
                (
                    n.id.clone(),
                    n.tag.strip_prefix("chat:").map(str::to_string),
                    n.at_ms,
                    if n.urgent || n.id.starts_with("failed/") {
                        "needs_attention"
                    } else {
                        "completed"
                    }
                    .into(),
                )
            })
            .collect();
        for value in self.engine.store.list_records(COMPLETIONS, None)? {
            let item: Completion = serde_json::from_value(value)?;
            if now.saturating_sub(item.at_ms) > MAX_AGE_MS {
                self.engine.store.delete_record(COMPLETIONS, &item.id)?;
            } else {
                source.push((item.id, Some(item.chat), item.at_ms, item.kind));
            }
        }
        let mut pending = false;
        for value in self.engine.store.list_records(DELIVERIES, None)? {
            let d: Delivery = serde_json::from_value(value)?;
            if now_seconds().saturating_sub(d.body.expiry) > 86_400 {
                self.engine
                    .store
                    .delete_record(DELIVERIES, &d.body.request_id)?;
            }
        }
        for value in self.engine.store.list_records(REGISTRATIONS, None)? {
            let r: Registration = serde_json::from_value(value)?;
            if r.route != c.route
                || r.expires <= now_seconds()
                || !tokens.external_client_hash_live(&r.owner)
            {
                self.remove(&r.owner, &r.id)?;
                continue;
            }
            let in_view = self
                .presence
                .lock()
                .unwrap()
                .get(&r.id)
                .is_some_and(|until| *until > now);
            for (notice, chat, at_ms, kind) in &source {
                if *at_ms < r.added_ms || now.saturating_sub(*at_ms) > MAX_AGE_MS {
                    continue;
                }
                if in_view || now.saturating_sub(*at_ms) < wait_ms {
                    pending = true;
                    continue;
                }
                let key = digest(&["delivery", &r.id, &r.owner, notice]);
                let old = self.engine.store.get_record(DELIVERIES, &key)?;
                let mut delivery = match old {
                    Some(value) => serde_json::from_value::<Delivery>(value)?,
                    None => Delivery {
                        registration: r.id.clone(),
                        owner: r.owner.clone(),
                        notice_id: notice.clone(),
                        chat_id: chat.clone(),
                        body: DeliveryBody {
                            request_id: key.clone(),
                            attention_id: digest(&["attention", &key]),
                            collapse_id: digest(&[
                                "collapse",
                                &r.id,
                                chat.as_deref().unwrap_or(notice),
                            ]),
                            expiry: r.expires.min(c.until).min(now_seconds() + 3600),
                            kind: kind.clone(),
                        },
                        state: "retry".into(),
                        attempts: 0,
                        next_attempt: 0,
                    },
                };
                if delivery.state != "retry"
                    || delivery.attempts >= 3
                    || delivery.body.expiry <= now_seconds()
                {
                    continue;
                }
                if delivery.next_attempt > now_seconds() {
                    pending = true;
                    continue;
                }
                // A crash or ambiguous transport result cannot prove the relay did not accept it.
                delivery.state = "unknown".into();
                delivery.attempts += 1;
                self.save(&key, &delivery)?;
                if !tokens.external_client_hash_live(&r.owner)
                    || self.registration(&r.id)?.is_none()
                {
                    continue;
                }
                let capability = self
                    .secrets()?
                    .get(&secret_key(&r.id))
                    .map_err(|_| anyhow::anyhow!("could not read notification capability"))?;
                let Some(capability) = capability else {
                    continue;
                };
                let url = format!("{}/v1/mobile-push/deliveries/{}", c.origin, r.id);
                if let Some(receipt) = self.send(&url, &capability, &delivery.body).await {
                    if uuid::Uuid::parse_str(&receipt.receipt_id).is_ok() {
                        match receipt.state.as_str() {
                            "accepted" | "unknown" | "refused" | "sending" => {
                                delivery.state = receipt.state
                            }
                            "retry" if delivery.attempts < 3 => {
                                delivery.state = "retry".into();
                                delivery.next_attempt = now_seconds() + 60;
                                pending = true;
                            }
                            _ => {}
                        }
                    }
                }
                self.save(&key, &delivery)?;
            }
        }
        Ok(pending)
    }

    fn save(&self, key: &str, delivery: &Delivery) -> anyhow::Result<()> {
        self.engine
            .store
            .put_record(DELIVERIES, key, "", &serde_json::to_value(delivery)?)?;
        Ok(())
    }

    async fn send(&self, url: &str, capability: &str, body: &DeliveryBody) -> Option<Receipt> {
        #[cfg(test)]
        if let Some(out) = self.out.get() {
            return Some(out(body));
        }
        let mut response = self
            .client
            .post(url)
            .bearer_auth(capability)
            .json(body)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len() + chunk.len() > 4096 {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).ok()
    }
}

fn digest(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
fn secret_key(id: &str) -> String {
    format!("native-push/{id}/send")
}
fn opaque(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn timestamp(value: &str) -> anyhow::Result<u64> {
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .and_then(|v| u64::try_from(v.unix_timestamp()).ok())
        .ok_or_else(|| anyhow::anyhow!("invalid notification expiry"))
}
fn now_seconds() -> u64 {
    crate::notify::now_ms() / 1000
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_sdk::Ark;
    use std::time::Duration;

    fn setup() -> (tempfile::TempDir, NativePush, Tokens, String) {
        let dir = tempfile::tempdir().unwrap();
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(Arc::new(ark_sdk::prelude::ark_model::MockProvider::new(
                "m",
                vec![],
            )))
            .build()
            .unwrap();
        let tokens = Tokens::in_store(dir.path());
        let client = tokens
            .issue_external_client("owner", "phone", "phone-a", Duration::from_secs(3600))
            .unwrap();
        let owner = format!("{:x}", Sha256::digest(client.token.as_bytes()));
        let secrets =
            Arc::new(ark_sdk::prelude::ark_connect::FileSecrets::in_store(dir.path()).unwrap());
        let push = NativePush::new(ark.shared_engine(), Some(secrets));
        assert!(push
            .config
            .set(Config {
                origin: "https://relay.example.com".into(),
                route: "route-a".into(),
                until: now_seconds() + 3600,
            })
            .is_ok());
        (dir, push, tokens, owner)
    }

    fn request() -> ark_protocol::NativePushRegistrationRequest {
        ark_protocol::NativePushRegistrationRequest {
            registration_id: uuid::Uuid::new_v4().to_string(),
            runtime_route_id: "route-a".into(),
            expires_at: OffsetDateTime::from_unix_timestamp((now_seconds() + 1800) as i64)
                .unwrap()
                .format(&Rfc3339)
                .unwrap(),
            send_capability: "A".repeat(43),
        }
    }

    fn notice(at_ms: u64) -> Notice {
        Notice {
            id: "private-question-id".into(),
            title: "DO NOT SEND TITLE".into(),
            body: "DO NOT SEND PROMPT OR FILES".into(),
            tag: "chat:chat-a".into(),
            urgent: true,
            at_ms,
        }
    }

    fn record_sends(push: &NativePush, state: &'static str) -> Arc<Mutex<Vec<DeliveryBody>>> {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let saved = sent.clone();
        assert!(push
            .out
            .set(Arc::new(move |body| {
                saved.lock().unwrap().push(body.clone());
                Receipt {
                    receipt_id: uuid::Uuid::new_v4().to_string(),
                    state: state.into(),
                }
            }))
            .is_ok());
        sent
    }

    #[test]
    fn fixed_https_origin_and_finite_expiry_are_required() {
        let until = OffsetDateTime::from_unix_timestamp((now_seconds() + 3600) as i64)
            .unwrap()
            .format(&Rfc3339)
            .unwrap();
        for origin in [
            "http://relay.example.com",
            "https://127.0.0.1",
            "https://[::1]",
            "https://localhost",
            "https://box.local",
            "https://user:password@relay.example.com",
            "https://relay.example.com/other",
            "https://relay.example.com?secret=x",
            "https://relay.example.com:444",
        ] {
            assert!(
                Config::parse(origin, "route-a", &until).is_err(),
                "{origin}"
            );
        }
        assert!(Config::parse("https://relay.example.com", "route-a", &until).is_ok());
        assert!(Config::parse(
            "https://relay.example.com",
            "route-a",
            "2000-01-01T00:00:00Z"
        )
        .is_err());
    }

    #[test]
    fn registration_is_bound_encrypted_and_redacted() {
        let (_dir, push, _tokens, owner) = setup();
        let input = request();
        let id = input.registration_id.clone();
        assert!(!format!("{input:?}").contains(&input.send_capability));
        assert!(push.register(&owner, "route-b", input.clone()).is_err());
        push.register(&owner, "route-a", input.clone()).unwrap();
        assert!(push
            .register("another-phone", "route-a", input.clone())
            .is_err());
        assert!(push.remove("another-phone", &id).is_err());
        let metadata = push
            .engine
            .store
            .get_record(REGISTRATIONS, &id)
            .unwrap()
            .unwrap()
            .to_string();
        assert!(!metadata.contains(&input.send_capability));
        let encrypted = std::fs::read(push.engine.store.root().join("secrets.enc")).unwrap();
        assert!(!String::from_utf8_lossy(&encrypted).contains(&input.send_capability));
        assert!(push
            .secrets()
            .unwrap()
            .get(&secret_key(&id))
            .unwrap()
            .is_some());
        push.remove(&owner, &id).unwrap();
        assert!(push
            .secrets()
            .unwrap()
            .get(&secret_key(&id))
            .unwrap()
            .is_none());
    }

    #[test]
    fn shared_secret_mutations_preserve_unrelated_connections() {
        let (_dir, push, _tokens, owner) = setup();
        let secrets = push.secrets.clone().unwrap();
        let writer = std::thread::spawn(move || {
            for n in 0..30 {
                secrets
                    .set(&format!("connection-{n}"), "private-refresh-token")
                    .unwrap();
            }
        });
        for _ in 0..30 {
            let input = request();
            let id = input.registration_id.clone();
            push.register(&owner, "route-a", input).unwrap();
            push.remove(&owner, &id).unwrap();
        }
        writer.join().unwrap();
        for n in 0..30 {
            assert_eq!(
                push.secrets()
                    .unwrap()
                    .get(&format!("connection-{n}"))
                    .unwrap()
                    .as_deref(),
                Some("private-refresh-token")
            );
        }
    }

    #[tokio::test]
    async fn one_opaque_event_once_and_only_its_phone_can_resolve() {
        let (_dir, push, tokens, owner) = setup();
        let input = request();
        push.register(&owner, "route-a", input).unwrap();
        let sent = record_sends(&push, "accepted");
        let notices = [notice(crate::notify::now_ms())];
        push.look(&notices, Some(&tokens), 0).await.unwrap();
        push.look(&notices, Some(&tokens), 0).await.unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].kind, "needs_attention");
        let encoded = serde_json::to_value(&sent[0]).unwrap();
        assert_eq!(encoded.as_object().unwrap().len(), 5);
        assert!(!encoded.to_string().contains("DO NOT SEND"));
        assert!(!encoded.to_string().contains("private-question-id"));
        assert!(!encoded.to_string().contains("chat-a"));
        assert_eq!(
            push.resolve(&owner, &sent[0].attention_id).unwrap(),
            Some(("private-question-id".into(), Some("chat-a".into())))
        );
        assert!(push
            .resolve("other-phone", &sent[0].attention_id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn revocation_foreground_and_historical_items_do_not_send() {
        let (_dir, push, tokens, owner) = setup();
        let input = request();
        let id = input.registration_id.clone();
        push.register(&owner, "route-a", input).unwrap();
        let sent = record_sends(&push, "accepted");
        push.look(&[notice(1)], Some(&tokens), 0).await.unwrap();
        assert!(sent.lock().unwrap().is_empty());
        push.present(
            &owner,
            &ark_protocol::NativePushPresenceRequest {
                registration_id: id.clone(),
                in_foreground: true,
            },
        )
        .unwrap();
        let notices = [notice(crate::notify::now_ms())];
        assert!(push.look(&notices, Some(&tokens), 0).await.unwrap());
        assert!(sent.lock().unwrap().is_empty());
        tokens.revoke("owner").unwrap();
        push.look(&notices, Some(&tokens), 0).await.unwrap();
        assert!(sent.lock().unwrap().is_empty());
        assert!(push.registration(&id).unwrap().is_none());
        assert!(push
            .secrets()
            .unwrap()
            .get(&secret_key(&id))
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn uncertain_and_sending_receipts_are_not_retried() {
        for state in ["unknown", "sending", "refused"] {
            let (_dir, push, tokens, owner) = setup();
            push.register(&owner, "route-a", request()).unwrap();
            let sent = record_sends(&push, state);
            let notices = [notice(crate::notify::now_ms())];
            push.look(&notices, Some(&tokens), 0).await.unwrap();
            push.look(&notices, Some(&tokens), 0).await.unwrap();
            assert_eq!(sent.lock().unwrap().len(), 1, "{state}");
        }
    }

    #[tokio::test]
    async fn explicit_retry_reuses_every_field_and_is_bounded() {
        let (_dir, push, tokens, owner) = setup();
        push.register(&owner, "route-a", request()).unwrap();
        let sent = record_sends(&push, "retry");
        let notices = [notice(crate::notify::now_ms())];
        for _ in 0..5 {
            push.look(&notices, Some(&tokens), 0).await.unwrap();
            for value in push.engine.store.list_records(DELIVERIES, None).unwrap() {
                let mut d: Delivery = serde_json::from_value(value).unwrap();
                d.next_attempt = 0;
                push.save(&d.body.request_id, &d).unwrap();
            }
        }
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 3);
        assert!(sent.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[tokio::test]
    async fn chat_finish_has_no_content_and_survives_notifier_restart() {
        let (_dir, push, tokens, owner) = setup();
        push.register(&owner, "route-a", request()).unwrap();
        push.completed("chat-a", Some(7), true).unwrap();
        let engine = push.engine.clone();
        let config = push.config.clone();
        let secrets = push.secrets.clone();
        drop(push);
        let mut restarted = NativePush::new(engine, secrets);
        restarted.config = config;
        let sent = record_sends(&restarted, "accepted");
        restarted.look(&[], Some(&tokens), 0).await.unwrap();
        restarted.look(&[], Some(&tokens), 0).await.unwrap();
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert_eq!(sent.lock().unwrap()[0].kind, "completed");
    }

    #[tokio::test]
    async fn http_registration_requires_current_external_identity_and_exact_route() {
        http_registration_surface(false).await;
    }

    #[tokio::test]
    async fn account_bound_headless_surface_supports_push_without_owner_authority() {
        http_registration_surface(true).await;
    }

    #[tokio::test]
    async fn headless_restarts_reuse_the_session_but_native_open_stays_new() {
        use crate::ArkUi;
        use ark_interact::App;
        let dir = tempfile::tempdir().unwrap();
        let app = || {
            App::new("Test").ark(
                Ark::builder()
                    .store_root(dir.path())
                    .provider(Arc::new(ark_sdk::prelude::ark_model::MockProvider::new(
                        "m",
                        vec![],
                    )))
                    .build()
                    .unwrap(),
            )
        };
        let mut first_id = None;
        for _ in 0..3 {
            let (host, _) = ArkUi::new(app())
                .serve_background("127.0.0.1:0")
                .await
                .unwrap();
            let sessions = host.engine_for_tests().store.list_sessions(None).unwrap();
            assert_eq!(sessions.len(), 1);
            if let Some(first_id) = &first_id {
                assert_eq!(&sessions[0]["id"], first_id);
            } else {
                first_id = Some(sessions[0]["id"].clone());
            }
        }
        for count in [2, 3] {
            let (host, _) = ArkUi::new(app()).open().await.unwrap();
            assert_eq!(
                host.engine_for_tests()
                    .store
                    .list_sessions(None)
                    .unwrap()
                    .len(),
                count
            );
        }
    }

    struct RefusePairing;

    #[async_trait::async_trait]
    impl ark_account::pairing::PairingAssertionVerifier for RefusePairing {
        async fn verify(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<
            ark_account::pairing::VerifiedRuntimePairing,
            ark_account::pairing::PairingVerificationError,
        > {
            Err(ark_account::pairing::PairingVerificationError::Assertion)
        }
    }

    async fn http_registration_surface(headless: bool) {
        use crate::{Access, AccountPairing, ArkUi, ClientListener};
        use ark_interact::App;
        let dir = tempfile::tempdir().unwrap();
        let secrets =
            Arc::new(ark_sdk::prelude::ark_connect::FileSecrets::in_store(dir.path()).unwrap());
        let ark = Ark::builder()
            .store_root(dir.path())
            .connections(true)
            .secrets(secrets)
            .provider(Arc::new(ark_sdk::prelude::ark_model::MockProvider::new(
                "m",
                vec![],
            )))
            .build()
            .unwrap();
        let tokens = Tokens::in_store(dir.path());
        let ui = ArkUi::new(App::new("Test").ark(ark));
        let (host, port, _server) = if headless {
            let pairing = AccountPairing::new("route-a", "owner", Arc::new(RefusePairing)).unwrap();
            let (host, server) = ui
                .access(Access::Tokens(tokens.clone()))
                .account_pairing(pairing)
                .serve_background("127.0.0.1:0")
                .await
                .unwrap();
            (host, server.port, None)
        } else {
            let (host, _) = ui.open().await.unwrap();
            let server = ClientListener::new(tokens.clone())
                .route_id("route-a")
                .unwrap()
                .serve(host.clone(), "127.0.0.1:0")
                .await
                .unwrap();
            (host, server.port, Some(server))
        };
        assert!(host
            .notifier
            .native
            .config
            .set(Config {
                origin: "https://relay.example.com".into(),
                route: "route-a".into(),
                until: now_seconds() + 3600,
            })
            .is_ok());
        let owner = tokens.create("owner").unwrap();
        let first = tokens
            .issue_external_client(
                "owner",
                "one",
                "installation-one",
                Duration::from_secs(3600),
            )
            .unwrap();
        let second = tokens
            .issue_external_client(
                "owner",
                "two",
                "installation-two",
                Duration::from_secs(3600),
            )
            .unwrap();
        let client = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{port}");
        let url = format!("{base}/client/v1/push");
        let input = request();
        for rejected in [
            owner.clone(),
            "account-jwt-is-not-a-runtime-credential".into(),
            String::new(),
        ] {
            let response = client
                .put(&url)
                .bearer_auth(rejected)
                .json(&input)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 401);
        }
        let mut wrong_route = input.clone();
        wrong_route.runtime_route_id = "route-b".into();
        assert_eq!(
            client
                .put(&url)
                .bearer_auth(&first.token)
                .json(&wrong_route)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        let response = client
            .put(&url)
            .bearer_auth(&first.token)
            .json(&input)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let response = response.text().await.unwrap();
        assert!(!response.contains(&input.send_capability));
        let hello: serde_json::Value = client
            .get(format!("{base}/client/v1/hello"))
            .bearer_auth(&first.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(hello["capabilities"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("push.native")));
        let presence = ark_protocol::NativePushPresenceRequest {
            registration_id: input.registration_id.clone(),
            in_foreground: true,
        };
        assert_eq!(
            client
                .post(format!("{url}/presence"))
                .bearer_auth(&first.token)
                .json(&presence)
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        assert_eq!(
            client
                .post(format!("{url}/presence"))
                .bearer_auth(&owner)
                .json(&presence)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        let resolve = format!("{url}/resolve/{}", "a".repeat(64));
        assert_eq!(
            client
                .get(&resolve)
                .bearer_auth(&first.token)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            client
                .get(&resolve)
                .bearer_auth(&owner)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        if headless {
            assert_eq!(
                client
                    .post(format!("{base}/rpc"))
                    .bearer_auth(&first.token)
                    .json(&serde_json::json!({"method":"hello", "params":{}}))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                401
            );
        }
        assert_eq!(
            client
                .put(&url)
                .bearer_auth(&second.token)
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        assert_eq!(
            client
                .delete(&url)
                .bearer_auth(&second.token)
                .json(&ark_protocol::NativePushRemoveRequest {
                    registration_id: input.registration_id.clone()
                })
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        assert_eq!(
            client
                .delete(&url)
                .bearer_auth(&first.token)
                .json(&ark_protocol::NativePushRemoveRequest {
                    registration_id: input.registration_id.clone()
                })
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        tokens.end(&first.token).unwrap();
        assert_eq!(
            client
                .put(&url)
                .bearer_auth(&first.token)
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
}
