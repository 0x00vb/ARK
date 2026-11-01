use super::*;
use crate::webhook::{self, Authentication, Challenge, ChallengeReply, Mode};
use ark_interact::ark_orchestrate::automation::{DeliveryState, WebhookDelivery, WebhookEndpoint};
use ark_sdk::prelude::ark_connect::{FileSecrets, Secrets};
use ark_sdk::prelude::ark_tools::events::WebhookSource;
use ring::rand::SecureRandom;

#[derive(Default)]
struct Listeners {
    count: usize,
    ports: HashMap<u16, usize>,
    origins: HashMap<String, usize>,
}
struct Ingress {
    bucket: Mutex<(f64, std::time::Instant)>,
    concurrent: Arc<tokio::sync::Semaphore>,
}
impl Default for Ingress {
    fn default() -> Self {
        Self {
            bucket: Mutex::new((16.0, std::time::Instant::now())),
            concurrent: Arc::new(tokio::sync::Semaphore::new(8)),
        }
    }
}
impl Ingress {
    fn admit(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.admit_at(std::time::Instant::now())
    }
    fn admit_at(&self, now: std::time::Instant) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let mut bucket = self.bucket.lock().unwrap();
        let now = now.max(bucket.1);
        bucket.0 = (bucket.0 + now.duration_since(bucket.1).as_secs_f64() * 8.0).min(16.0);
        bucket.1 = now;
        if bucket.0 < 1.0 {
            return None;
        }
        bucket.0 -= 1.0;
        Arc::clone(&self.concurrent).try_acquire_owned().ok()
    }
}

#[derive(Default)]
pub(super) struct State {
    ingress: Ingress,
    listeners: Mutex<Listeners>,
    rejected: Mutex<HashMap<String, u64>>,
}

pub(crate) struct ListenerLease {
    host: Arc<Host>,
    port: u16,
    origin: Option<String>,
}
impl Drop for ListenerLease {
    fn drop(&mut self) {
        let mut listeners = self.host.webhooks.listeners.lock().unwrap();
        listeners.count = listeners.count.saturating_sub(1);
        if let Some(count) = listeners.ports.get_mut(&self.port) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                listeners.ports.remove(&self.port);
            }
        }
        if let Some(origin) = &self.origin {
            if let Some(count) = listeners.origins.get_mut(origin) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    listeners.origins.remove(origin);
                }
            }
        }
    }
}

pub(crate) struct HttpReply {
    pub status: &'static str,
    pub body: Vec<u8>,
}
fn reply(status: &'static str, value: Value) -> anyhow::Result<HttpReply> {
    Ok(HttpReply {
        status,
        body: serde_json::to_vec(&value)?,
    })
}
fn now_ms() -> anyhow::Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}
fn secret_name(connection: &str) -> String {
    format!("automation-webhook/{connection}")
}
static SECRET_OPERATIONS: Mutex<()> = Mutex::new(());

impl Host {
    pub(crate) fn admit_webhook_http(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.webhooks.ingress.admit()
    }

    pub(crate) fn register_webhook_listener(
        self: &Arc<Self>,
        port: u16,
        origin: Option<&str>,
    ) -> anyhow::Result<ListenerLease> {
        let origin = origin
            .map(webhook_base)
            .transpose()?
            .map(|url| url.origin().ascii_serialization());
        let mut listeners = self.webhooks.listeners.lock().unwrap();
        listeners.count += 1;
        if port > 0 {
            *listeners.ports.entry(port).or_default() += 1;
        }
        if let Some(origin) = &origin {
            *listeners.origins.entry(origin.clone()).or_default() += 1;
        }
        Ok(ListenerLease {
            host: self.clone(),
            port,
            origin,
        })
    }

    fn webhook_secret(&self, connection: &str, create: bool) -> anyhow::Result<String> {
        let _operation = SECRET_OPERATIONS.lock().unwrap();
        let secrets: Arc<dyn Secrets> = match &self.caps.connections {
            Some(connections) => connections.secret_store(),
            None => Arc::new(FileSecrets::in_store(self.engine.store.root())?),
        };
        let name = secret_name(connection);
        if let Some(secret) = secrets.get(&name)? {
            return Ok(secret);
        }
        anyhow::ensure!(create, "the webhook credential needs setup recovery");
        let mut random = [0u8; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| anyhow::anyhow!("a secure webhook credential could not be generated"))?;
        use base64::Engine as _;
        let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
        secrets.set(&name, &secret)?;
        Ok(secret)
    }

    pub(super) fn webhook_options(&self) -> Value {
        let listeners = self.webhooks.listeners.lock().unwrap();
        let mut bases = listeners
            .origins
            .keys()
            .map(|origin| format!("{origin}/"))
            .collect::<Vec<_>>();
        bases.extend(
            listeners
                .ports
                .keys()
                .map(|port| format!("http://127.0.0.1:{port}/")),
        );
        bases.sort();
        json!({"available":listeners.count>0,"bases":bases})
    }

    pub(super) fn webhook_setup(&self, o: &Orchestrator, input: &Value) -> anyhow::Result<Value> {
        anyhow::ensure!(
            self.webhooks.listeners.lock().unwrap().count > 0,
            "No webhook connection is available on this runtime."
        );
        let base = webhook_base(input["base"].as_str().unwrap_or_default())?;
        let receipt = o.configure_webhook(
            input["automation"].as_str().unwrap_or_default(),
            input["expected"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("which revision?"))?,
            input["request"].as_str().unwrap_or_default(),
            base.as_str(),
            input["max_per_hour"].as_u64().unwrap_or(60).try_into()?,
        )?;
        let secret = self.webhook_secret(&receipt.connection.id, true)?;
        let endpoint = o.webhook_endpoint(&receipt.endpoint)?;
        Ok(json!({"receipt":receipt,"endpoint":endpoint,"secret":secret}))
    }

    pub(super) async fn webhook_probe(
        &self,
        o: &Orchestrator,
        id: &str,
        connection: &str,
    ) -> anyhow::Result<WebhookEndpoint> {
        let endpoint = o.webhook_endpoint(id)?;
        anyhow::ensure!(
            endpoint.draft.id == connection,
            "The webhook connection changed. Reload it before checking."
        );
        let checked = async {
            let secret = self.webhook_secret(connection, false)?;
            let auth = Authentication {
                endpoint: id,
                connection,
                secret: &secret,
            };
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            let body = serde_json::to_vec(&Challenge {
                challenge: nonce.clone(),
            })?;
            let headers = webhook::signed_headers(&auth, Mode::Preview, now_ms()?, &nonce, &body);
            let url = url::Url::parse(&endpoint.draft.url)?;
            anyhow::ensure!(
                url.path() == format!("/webhooks/v1/{id}"),
                "The webhook address names another endpoint."
            );
            let (client, local) = self.webhook_probe_client(&url).await?;
            let request = client
                .post(url)
                .header("content-type", "application/json")
                .body(body);
            let request = headers.into_iter().fold(request, |request, (name, value)| {
                request.header(name, value)
            });
            let mut response = request.send().await?;
            anyhow::ensure!(
                response.status().is_success(),
                "The webhook connection did not answer its preview."
            );
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                anyhow::ensure!(
                    body.len() + chunk.len() <= 8192,
                    "The webhook preview response is too large."
                );
                body.extend_from_slice(&chunk);
            }
            let proof: ChallengeReply = serde_json::from_slice(&body)?;
            webhook::verify_preview_reply(&auth, &nonce, &nonce, &proof)?;
            Ok::<_, anyhow::Error>(local)
        }
        .await;
        match checked {
            Ok(local) => Ok(o.record_webhook_probe(id, connection, local)?),
            Err(error) => {
                if let Err(storage) = o.record_webhook_probe_failure(id, connection) {
                    return Err(anyhow::anyhow!("The connection check failed: {error}. Its status could not be saved: {storage}"));
                }
                Err(error)
            }
        }
    }

    async fn webhook_probe_client(
        &self,
        url: &url::Url,
    ) -> anyhow::Result<(reqwest::Client, bool)> {
        let mut base = url.clone();
        base.set_path("/");
        webhook_base(base.as_str())?;
        let local = matches!(url.host(),Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(url.host(),Some(url::Host::Ipv6(ip)) if ip.is_loopback());
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow::anyhow!("The webhook address needs a port."))?;
        let origin = url.origin().ascii_serialization();
        let trusted = {
            let listeners = self.webhooks.listeners.lock().unwrap();
            anyhow::ensure!(listeners.count > 0, "The runtime has no webhook listener.");
            if local {
                anyhow::ensure!(
                    url.scheme() == "http" && listeners.ports.contains_key(&port),
                    "Use this runtime's actual local connection."
                );
            }
            listeners.origins.contains_key(&origin)
        };
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("The webhook address needs a host."))?;
        let addresses = match url.host() {
            Some(url::Host::Ipv4(ip)) => vec![std::net::SocketAddr::new(ip.into(), port)],
            Some(url::Host::Ipv6(ip)) => vec![std::net::SocketAddr::new(ip.into(), port)],
            Some(url::Host::Domain(domain)) => {
                tokio::net::lookup_host((domain, port)).await?.collect()
            }
            None => anyhow::bail!("The webhook address needs a host."),
        };
        anyhow::ensure!(
            !addresses.is_empty() && addresses.len() <= 64,
            "The webhook address did not resolve to a bounded destination."
        );
        if !local && !trusted {
            anyhow::ensure!(
                addresses.iter().all(|address| public_address(address.ip())),
                "Choose a public HTTPS address or an existing trusted runtime connection."
            );
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(8))
            .resolve_to_addrs(host, &addresses)
            .build()?;
        Ok((client, local))
    }

    pub(crate) async fn receive_webhook(
        self: &Arc<Self>,
        id: &str,
        raw_headers: &[(String, String)],
        body: &[u8],
    ) -> anyhow::Result<HttpReply> {
        let Some(endpoint) = Orchestrator::webhook_route(&self.engine.store, id)? else {
            return reply("401 Unauthorized", json!({"error":"Request not verified"}));
        };
        let o = self
            .siblings
            .project_orchestrator(&ProjectId(endpoint.project.clone()))?;
        let mut headers = std::collections::BTreeMap::new();
        for (name, value) in raw_headers {
            let name = name.to_ascii_lowercase();
            if name.starts_with("x-ark-") && headers.insert(name, value.clone()).is_some() {
                return reply("401 Unauthorized", json!({"error":"Request not verified"}));
            }
        }
        let mode = match headers.get("x-ark-mode").map(String::as_str) {
            Some("preview") => Mode::Preview,
            Some("deliver") => Mode::Deliver,
            _ => return reply("401 Unauthorized", json!({"error":"Request not verified"})),
        };
        let requested = headers
            .get("x-ark-connection")
            .map(String::as_str)
            .unwrap_or_default();
        let connection = match mode {
            Mode::Preview if endpoint.draft.id == requested => Some(&endpoint.draft),
            Mode::Deliver => endpoint
                .active
                .as_ref()
                .filter(|active| active.id == requested),
            _ => None,
        };
        let Some(connection) = connection else {
            return reply("401 Unauthorized", json!({"error":"Request not verified"}));
        };
        let secret = self.webhook_secret(&connection.id, false)?;
        let auth = Authentication {
            endpoint: id,
            connection: &connection.id,
            secret: &secret,
        };
        let now = now_ms()?;
        let verified = match webhook::authenticate(&auth, &headers, body, mode, now) {
            Ok(verified) => verified,
            Err(_) => {
                let log = {
                    let mut rejected = self.webhooks.rejected.lock().unwrap();
                    let prior = rejected.entry(id.into()).or_default();
                    if now.saturating_sub(*prior) >= 1000 {
                        *prior = now;
                        true
                    } else {
                        false
                    }
                };
                if log {
                    o.record_webhook_delivery(
                        id,
                        WebhookDelivery {
                            at_ms: now,
                            state: DeliveryState::Rejected,
                            reason: Some("Request not verified".into()),
                            run: None,
                        },
                    )?;
                }
                return reply("401 Unauthorized", json!({"error":"Request not verified"}));
            }
        };
        if mode == Mode::Preview {
            let challenge: Challenge = match serde_json::from_slice(body) {
                Ok(challenge) => challenge,
                Err(_) => return reply("400 Bad Request", json!({"error":"Invalid preview"})),
            };
            let proof = webhook::preview_reply(&auth, &verified.event, &challenge.challenge)?;
            o.record_webhook_delivery(
                id,
                WebhookDelivery {
                    at_ms: now,
                    state: DeliveryState::Preview,
                    reason: None,
                    run: None,
                },
            )?;
            return reply("200 OK", serde_json::to_value(proof)?);
        }
        let source = webhook::Source {
            auth,
            account: &endpoint.account,
            automation: &endpoint.automation,
            now,
        };
        let event = match source.decode(&headers, body) {
            Ok(mut events) => events
                .pop()
                .ok_or_else(|| anyhow::anyhow!("The verified webhook produced no event."))?,
            Err(_) => {
                o.record_webhook_delivery(
                    id,
                    WebhookDelivery {
                        at_ms: now,
                        state: DeliveryState::Rejected,
                        reason: Some("Invalid input body".into()),
                        run: None,
                    },
                )?;
                return reply("400 Bad Request", json!({"error":"Invalid input body"}));
            }
        };
        let inputs: webhook::Inputs = serde_json::from_str(
            event
                .payload
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("The verified webhook produced no input."))?,
        )?;
        match o.admit_webhook_event(id, &connection.id, &event.key, event.at_ms, inputs.inputs) {
            Ok(admitted) => {
                o.record_webhook_delivery(
                    id,
                    WebhookDelivery {
                        at_ms: now,
                        state: if admitted.duplicate {
                            DeliveryState::Duplicate
                        } else {
                            DeliveryState::Admitted
                        },
                        reason: None,
                        run: Some(admitted.run.id.clone()),
                    },
                )?;
                o.execute_automation_run(&admitted.run.id)?;
                reply(
                    "202 Accepted",
                    json!({"run":admitted.run.id,"work":admitted.run.work,"conversation":admitted.run.conversation,"duplicate":admitted.duplicate}),
                )
            }
            Err(error) => {
                let technical = !matches!(
                    &error,
                    ark_interact::ark_orchestrate::OrchError::NotAllowed(_)
                );
                if technical {
                    tracing::error!(endpoint=%id,%error,"webhook admission failed");
                }
                let limited = !technical && error.to_string().contains("limit");
                o.record_webhook_delivery(
                    id,
                    WebhookDelivery {
                        at_ms: now,
                        state: DeliveryState::Rejected,
                        reason: Some(
                            if limited {
                                "Admission limit reached"
                            } else {
                                "The event was not admitted"
                            }
                            .into(),
                        ),
                        run: None,
                    },
                )?;
                reply(
                    if technical {
                        "503 Service Unavailable"
                    } else if limited {
                        "429 Too Many Requests"
                    } else {
                        "409 Conflict"
                    },
                    json!({"error":if limited { "Admission limit reached" } else { "The event was not admitted" }}),
                )
            }
        }
    }
}

pub(crate) fn webhook_base(value: &str) -> anyhow::Result<url::Url> {
    anyhow::ensure!(value.len() <= 4096, "The webhook address is too long.");
    let url = url::Url::parse(value)?;
    let local = matches!(url.host(),Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(url.host(),Some(url::Host::Ipv6(ip)) if ip.is_loopback());
    anyhow::ensure!(url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none() && url.path()=="/" && (url.scheme()=="https" && url.port_or_known_default()==Some(443) || local && url.scheme()=="http"),"Use an HTTPS base on port443 or this runtime's local listener, without credentials or another path.");
    Ok(url)
}
fn public_address(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let octets = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && octets[0] != 0
                && octets[0] < 224
                && !(octets[0] == 100 && (64..=127).contains(&octets[1]))
                && !(octets[0] == 198 && (18..=19).contains(&octets[1]))
                && !(octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        }
        std::net::IpAddr::V6(ip) => {
            let segments = ip.segments();
            (segments[0] & 0xe000) == 0x2000
                && !(segments[0] == 0x2001 && (segments[1] < 0x200 || segments[1] == 0xdb8))
                && segments[0] != 0x2002
                && segments[0] != 0x3fff
        }
    }
}

#[cfg(test)]
mod ingress_tests {
    use super::*;

    #[tokio::test]
    async fn incomplete_and_invalid_requests_share_bounded_ingress_and_capacity_recovers() {
        let ingress = Ingress::default();
        let now = std::time::Instant::now();
        let mut active = (0..8)
            .map(|_| ingress.admit_at(now).unwrap())
            .collect::<Vec<_>>();
        assert!(ingress.admit_at(now).is_none());
        active.pop();
        assert!(ingress.admit_at(now).is_some());
        drop(active);
        for _ in 0..6 {
            assert!(ingress.admit_at(now).is_some());
        }
        assert!(ingress.admit_at(now).is_none());
        assert!(ingress
            .admit_at(now + std::time::Duration::from_millis(150))
            .is_some());
    }
}
