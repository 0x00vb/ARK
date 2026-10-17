//! A WhatsApp Business number through Meta's official Business Platform Cloud API (ADR 0062,
//! `docs/AUTOMATIONS.md` › WhatsApp): [`Hook`] verifies and decodes the webhooks Meta sends to
//! the public route the person supplies, and [`Reply`] answers a customer inside Meta's 24-hour
//! customer service window. A personal WhatsApp account is never an event source.
//!
//! | Rule | Why |
//! |---|---|
//! | The connection is saved only after Graph confirms the token, that the phone number is in the WABA, and that the WABA is subscribed to an app | a half set-up account never shows Ready |
//! | The signature (`X-Hub-Signature-256`, HMAC-SHA256 with the app secret) is checked over the raw bytes before any parsing | nothing is read from a body Meta did not sign |
//! | Every entry must name the connected WABA and phone number id | a signed body for another number is refused |
//! | A message's key is `wa:msg:<id>`; a status's is `wa:status:<id>:<status>:<time>` | a re-delivery keeps its key, a status change never hides a message |
//! | A contact's profile name is never used | anyone can set it (`Ignore all rules`) |
//! | A free-form reply needs a deadline before now and at most 24 hours ahead, checked at each send | after the window, it fails closed and never turns into a template |
//! | A send with no answer or a server error is an unconfirmed effect, never retried | the send API has no idempotency key |
//! | Secrets are read from the store on each request | rotating the token or app secret applies at once, the route stays |

use std::collections::BTreeMap;
use std::time::Duration;

use ark_core::{ActionDescriptor, ExternalEffect, ExternalEffectState, RiskTier};
use ark_tools::events::{Direction, ExternalEvent, HookError, Limits, WebhookSource};
use ark_tools::{ActionOutput, Adapter, AdapterError, Result, RunCtx};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{Account, ConnectError, Connections};

/// The provider id of WhatsApp Business accounts (`whatsapp:<waba>/<phone>`).
pub const PROVIDER: &str = "whatsapp";
/// The service name in `catalog.json`.
pub const SERVICE: &str = "whatsapp.business";
/// The event source name of WhatsApp webhooks.
pub const SOURCE: &str = "whatsapp";
pub const REPLY: &str = "whatsapp.reply";

const GRAPH: &str = "https://graph.facebook.com/v21.0";
/// Meta's customer service window after a customer's last message.
pub const WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
/// How long a send may take before its outcome counts as unknown.
const SEND_WAIT: Duration = Duration::from_secs(30);
/// A text message's body, at most (Meta's limit).
const TEXT_MAX: usize = 4096;

/// What a WhatsApp Business connection needs, from Meta's app dashboard and WhatsApp Manager.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    /// The WhatsApp Business Account id.
    pub waba_id: String,
    /// The business phone number's id (not the number).
    pub phone_id: String,
    /// The Meta app's secret, which signs webhooks.
    pub app_secret: String,
    /// The string the person typed as the webhook's verify token in the app dashboard.
    pub verify_token: String,
    /// A system user access token with `whatsapp_business_management` and
    /// `whatsapp_business_messaging`.
    pub token: String,
}

/// What is kept in the secret store for one connected number.
#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    #[serde(flatten)]
    setup: Setup,
    /// The opaque last part of the webhook path. Kept when the secrets change.
    route: String,
}

/// Where Meta sends this number's webhooks: the path to put after the person's public HTTPS
/// address, with the verify token they gave.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Hookup {
    pub path: String,
}

fn account_id(s: &Setup) -> String {
    format!("{PROVIDER}:{}/{}", s.waba_id, s.phone_id)
}

/// The account name events carry: `<waba>/<phone>`.
pub fn event_account(account_id: &str) -> Option<&str> {
    account_id.strip_prefix(&format!("{PROVIDER}:"))
}

fn plain_id(field: &str, v: &str) -> std::result::Result<(), ConnectError> {
    if v.is_empty()
        || !v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ConnectError::NotSetUp(format!(
            "the {field} is an id of letters and digits"
        )));
    }
    Ok(())
}

/// Check `setup` against Graph and save it. Connecting a number again (a new token or app
/// secret) keeps its webhook path, so Meta's settings stay valid.
pub async fn connect(
    conns: &Connections,
    setup: Setup,
) -> std::result::Result<(Account, Hookup), ConnectError> {
    plain_id("WhatsApp Business Account id", &setup.waba_id)?;
    plain_id("phone number id", &setup.phone_id)?;
    for (name, v) in [
        ("app secret", &setup.app_secret),
        ("verify token", &setup.verify_token),
        ("access token", &setup.token),
    ] {
        if v.trim().is_empty() || v.chars().any(char::is_control) {
            return Err(ConnectError::NotSetUp(format!("the {name} is missing")));
        }
    }
    let base = conns.base(SERVICE).unwrap_or(GRAPH).to_string();
    let get = |path: String| {
        let (http, token) = (conns.http().clone(), setup.token.clone());
        async move {
            let resp = http.get(path).bearer_auth(token).send().await?;
            let status = resp.status().as_u16();
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            Ok::<_, ConnectError>((status, body))
        }
    };
    let why = |b: &Value| {
        b["error"]["message"]
            .as_str()
            .unwrap_or("no reason given")
            .to_string()
    };
    let (status, number) = get(format!(
        "{base}/{}?fields=display_phone_number,verified_name",
        setup.phone_id
    ))
    .await?;
    // An invalid token is 401, or 400 with Graph's code 190.
    match status {
        200 => {}
        _ if status == 401 || number["error"]["code"].as_u64() == Some(190) => {
            return Err(ConnectError::Refused(format!(
                "Meta refused the access token: {}",
                why(&number)
            )))
        }
        _ => {
            return Err(ConnectError::NotSetUp(format!(
                "Meta does not show phone number id {} to this token: {}",
                setup.phone_id,
                why(&number)
            )))
        }
    }
    let (status, numbers) =
        get(format!("{base}/{}/phone_numbers?fields=id", setup.waba_id)).await?;
    let listed = numbers["data"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|n| n["id"].as_str() == Some(setup.phone_id.as_str()));
    if status != 200 || !listed {
        return Err(ConnectError::NotSetUp(format!(
            "phone number id {} is not in WhatsApp Business Account {}{}",
            setup.phone_id,
            setup.waba_id,
            if status == 200 {
                String::new()
            } else {
                format!(": {}", why(&numbers))
            }
        )));
    }
    let (status, apps) = get(format!("{base}/{}/subscribed_apps", setup.waba_id)).await?;
    if status != 200 || apps["data"].as_array().is_none_or(|a| a.is_empty()) {
        return Err(ConnectError::NotSetUp(format!(
            "WhatsApp Business Account {} sends its webhooks to no app; subscribe the app to it first",
            setup.waba_id
        )));
    }
    let display = number["display_phone_number"].as_str().unwrap_or("");
    let name = if display.is_empty() {
        format!("{}/{}", setup.waba_id, setup.phone_id)
    } else {
        format!("+{}", display.trim_start_matches('+'))
    };
    let id = account_id(&setup);
    let key = crate::connections::secret_key(&id);
    let route = conns
        .secrets()
        .get(&key)?
        .and_then(|t| serde_json::from_str::<Saved>(&t).ok())
        .map(|s| s.route)
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    let saved = Saved {
        setup,
        route: route.clone(),
    };
    conns.secrets().set(
        &key,
        &serde_json::to_string(&saved).expect("setup serializes"),
    )?;
    let account = Account {
        id,
        provider: PROVIDER.into(),
        name,
        scopes: Vec::new(),
        connected_at: time::OffsetDateTime::now_utc().unix_timestamp(),
    };
    conns.add_account(account.clone())?;
    Ok((
        account,
        Hookup {
            path: format!("/hooks/{SOURCE}/{route}"),
        },
    ))
}

fn saved(conns: &Connections, account: &str) -> Option<Saved> {
    let text = conns
        .secrets()
        .get(&crate::connections::secret_key(account))
        .ok()??;
    serde_json::from_str(&text).ok()
}

/// The webhook of the connected number whose route is `route`, if any.
pub fn hook(conns: &Connections, route: &str) -> Option<Hook> {
    use sha2::Digest as _;
    let want = sha2::Sha256::digest(route.as_bytes());
    conns
        .accounts()
        .into_iter()
        .filter(|a| a.provider == PROVIDER)
        .filter_map(|a| saved(conns, &a.id))
        .find(|s| sha2::Sha256::digest(s.route.as_bytes()) == want)
        .map(|s| Hook {
            account: format!("{}/{}", s.setup.waba_id, s.setup.phone_id),
            saved: s,
            limits: Limits::default(),
        })
}

/// Webhooks of one connected number.
pub struct Hook {
    saved: Saved,
    account: String,
    limits: Limits,
}

impl Hook {
    /// A hook for `setup` without a store (tests and the fixture check).
    pub fn new(setup: Setup) -> Self {
        Hook {
            account: format!("{}/{}", setup.waba_id, setup.phone_id),
            saved: Saved {
                setup,
                route: String::new(),
            },
            limits: Limits::default(),
        }
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    fn verify(
        &self,
        headers: &BTreeMap<String, String>,
        raw: &[u8],
    ) -> std::result::Result<(), HookError> {
        let sig = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("x-hub-signature-256"))
            .map(|(_, v)| v.trim())
            .ok_or_else(|| HookError::Unverified("no X-Hub-Signature-256".into()))?;
        let tag = sig
            .strip_prefix("sha256=")
            .and_then(unhex)
            .ok_or_else(|| HookError::Unverified("the signature is not sha256=<hex>".into()))?;
        let key = ring::hmac::Key::new(
            ring::hmac::HMAC_SHA256,
            self.saved.setup.app_secret.as_bytes(),
        );
        ring::hmac::verify(&key, raw, &tag).map_err(|_| {
            HookError::Unverified("the signature does not match the app secret".into())
        })
    }
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Constant time, so the verify token cannot be guessed a byte at a time.
fn same(a: &str, b: &str) -> bool {
    use sha2::Digest as _;
    sha2::Sha256::digest(a.as_bytes()) == sha2::Sha256::digest(b.as_bytes())
}

/// A WhatsApp id: the customer's number in digits.
fn wa_id(v: &str) -> bool {
    (5..=20).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_digit())
}

fn ms_of(timestamp: &Value) -> Option<u64> {
    timestamp.as_str()?.parse::<u64>().ok()?.checked_mul(1000)
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

impl WebhookSource for Hook {
    fn source(&self) -> &str {
        SOURCE
    }

    fn account(&self) -> &str {
        &self.account
    }

    fn challenge(
        &self,
        query: &BTreeMap<String, String>,
    ) -> std::result::Result<String, HookError> {
        let q = |k: &str| query.get(k).map(String::as_str).unwrap_or("");
        if q("hub.mode") != "subscribe"
            || !same(q("hub.verify_token"), &self.saved.setup.verify_token)
        {
            return Err(HookError::Unverified(
                "the verify token does not match".into(),
            ));
        }
        let challenge = q("hub.challenge");
        if challenge.is_empty()
            || challenge.len() > 256
            || !challenge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(HookError::Malformed(
                "the challenge is not a short token".into(),
            ));
        }
        Ok(challenge.to_string())
    }

    fn decode(
        &self,
        headers: &BTreeMap<String, String>,
        raw: &[u8],
    ) -> std::result::Result<Vec<ExternalEvent>, HookError> {
        if raw.len() > self.limits.raw_body {
            return Err(HookError::Malformed(format!(
                "a body of {} bytes; at most {} are read",
                raw.len(),
                self.limits.raw_body
            )));
        }
        self.verify(headers, raw)?;
        let body: Value = serde_json::from_slice(raw)
            .map_err(|e| HookError::Malformed(format!("not JSON: {e}")))?;
        if body["object"] != "whatsapp_business_account" {
            return Err(HookError::Malformed(
                "not a WhatsApp Business webhook".into(),
            ));
        }
        let (waba, phone) = (&self.saved.setup.waba_id, &self.saved.setup.phone_id);
        let bad = |why: &str| HookError::Malformed(why.to_string());
        // One Meta app sends every number's changes to its one callback: changes for another
        // number are left out, and a body with only those is refused.
        let mut out = Vec::new();
        let (mut ours, mut foreign) = (0, None);
        for entry in body["entry"].as_array().ok_or_else(|| bad("no entries"))? {
            if entry["id"].as_str() != Some(waba.as_str()) {
                foreign = Some(format!(
                    "WhatsApp Business Account {}",
                    entry["id"].as_str().unwrap_or("?")
                ));
                continue;
            }
            for change in entry["changes"].as_array().into_iter().flatten() {
                if change["field"] != "messages" {
                    ours += 1;
                    continue;
                }
                let v = &change["value"];
                let to = v["metadata"]["phone_number_id"].as_str();
                if to != Some(phone.as_str()) {
                    foreign = Some(format!("phone number id {}", to.unwrap_or("?")));
                    continue;
                }
                ours += 1;
                for m in v["messages"].as_array().into_iter().flatten() {
                    out.push(self.message(m)?);
                }
                for s in v["statuses"].as_array().into_iter().flatten() {
                    out.push(self.status(s)?);
                }
            }
        }
        if let (0, Some(other)) = (ours, foreign) {
            return Err(HookError::Unbound(format!("changes only for {other}")));
        }
        Ok(out)
    }
}

impl Hook {
    fn message(&self, m: &Value) -> std::result::Result<ExternalEvent, HookError> {
        let bad = |why: &str| HookError::Malformed(format!("a message {why}"));
        let id = m["id"]
            .as_str()
            .filter(|i| !i.is_empty() && i.len() <= 256 && !i.contains(char::is_control))
            .ok_or_else(|| bad("has no id"))?;
        let from = m["from"]
            .as_str()
            .filter(|f| wa_id(f))
            .ok_or_else(|| bad("is from no WhatsApp number"))?;
        let at_ms = ms_of(&m["timestamp"]).ok_or_else(|| bad("has no time"))?;
        let kind = m["type"]
            .as_str()
            .filter(|t| t.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') && !t.is_empty())
            .unwrap_or("unknown");
        let payload = match kind {
            "text" => m["text"]["body"].as_str(),
            "button" => m["button"]["text"].as_str(),
            "interactive" => m["interactive"]["button_reply"]["title"]
                .as_str()
                .or(m["interactive"]["list_reply"]["title"].as_str()),
            _ => None,
        }
        .map(|p| clip(p, self.limits.payload));
        let until = at_ms
            .checked_add(WINDOW_MS)
            .ok_or_else(|| bad("has a time past any window"))?;
        let fields = BTreeMap::from([
            ("message_id".to_string(), id.to_string()),
            ("from".to_string(), from.to_string()),
            ("type".to_string(), kind.to_string()),
            ("reply_until_ms".to_string(), until.to_string()),
        ]);
        Ok(ExternalEvent {
            source: SOURCE.into(),
            account: self.account.clone(),
            key: format!("wa:msg:{id}"),
            kind: "message".into(),
            direction: Direction::Inbound,
            actor: Some(from.into()),
            subject: Some(from.into()),
            at_ms,
            label: format!("WhatsApp message from +{from}"),
            fields,
            payload,
        })
    }

    fn status(&self, s: &Value) -> std::result::Result<ExternalEvent, HookError> {
        let bad = |why: &str| HookError::Malformed(format!("a status {why}"));
        let id = s["id"]
            .as_str()
            .filter(|i| !i.is_empty() && i.len() <= 256 && !i.contains(char::is_control))
            .ok_or_else(|| bad("has no message id"))?;
        let status = s["status"]
            .as_str()
            .filter(|t| !t.is_empty() && t.len() <= 32 && t.bytes().all(|b| b.is_ascii_lowercase()))
            .ok_or_else(|| bad("has no status"))?;
        let to = s["recipient_id"]
            .as_str()
            .filter(|f| wa_id(f))
            .ok_or_else(|| bad("has no recipient"))?;
        let at_ms = ms_of(&s["timestamp"]).ok_or_else(|| bad("has no time"))?;
        Ok(ExternalEvent {
            source: SOURCE.into(),
            account: self.account.clone(),
            key: format!("wa:status:{id}:{status}:{}", at_ms / 1000),
            kind: "status".into(),
            direction: Direction::Outbound,
            actor: Some(to.into()),
            subject: Some(to.into()),
            at_ms,
            label: format!("WhatsApp message to +{to} {status}"),
            fields: BTreeMap::from([
                ("message_id".to_string(), id.to_string()),
                ("status".to_string(), status.to_string()),
            ]),
            payload: None,
        })
    }
}

/// Replies from a connected number.
pub struct Reply(pub(super) super::Ctx);

#[async_trait]
impl Adapter for Reply {
    fn name(&self) -> &str {
        "whatsapp"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            REPLY,
            "answer a customer who wrote to the business's WhatsApp number, within 24 hours of their message",
            RiskTier::Mutate,
        )
        .with_input_schema(json!({
            "type": "object",
            "required": ["to", "text", "reply_until_ms"],
            "properties": {
                "to": { "type": "string", "description": "the customer's WhatsApp number in digits (the message's `from`)" },
                "text": { "type": "string", "description": "plain text" },
                "reply_until_ms": { "type": "string", "description": "the message's `reply_until_ms`" },
                "message_id": { "type": "string", "description": "the message answered, shown as quoted" }
            }
        }))]
    }

    async fn run(&self, action: &str, input: Value, _ctx: &RunCtx<'_>) -> Result<ActionOutput> {
        match action {
            REPLY => self.reply(&input).await,
            _ => Err(AdapterError::UnknownAction(action.into())),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Reply {
    async fn reply(&self, input: &Value) -> Result<ActionOutput> {
        let bad = |why: String| AdapterError::bad_input(REPLY, why);
        let to = input["to"].as_str().unwrap_or("").trim_start_matches('+');
        if !wa_id(to) {
            return Err(bad("`to` is a WhatsApp number in digits".into()));
        }
        let text = input["text"].as_str().unwrap_or("");
        if text.trim().is_empty() || text.chars().count() > TEXT_MAX {
            return Err(bad(format!("`text` is 1 to {TEXT_MAX} characters")));
        }
        let until = match &input["reply_until_ms"] {
            Value::String(s) => s.trim().parse::<u64>().ok(),
            v => v.as_u64(),
        }
        .ok_or_else(|| bad("`reply_until_ms` is the message's reply deadline".into()))?;
        let now = now_ms();
        if until > now + WINDOW_MS {
            return Err(bad("`reply_until_ms` is more than 24 hours ahead; it comes from the customer's message".into()));
        }
        if until <= now {
            return Err(AdapterError::Tool(window_ended(to)));
        }
        let quoted = input["message_id"]
            .as_str()
            .filter(|m| !m.is_empty() && m.len() <= 256 && !m.contains(char::is_control));
        let saved = saved(&self.0.conns, &self.0.account)
            .ok_or_else(|| AdapterError::Tool("WhatsApp Business needs connecting again".into()))?;
        let base = self.0.base(GRAPH).to_string();
        let mut body = json!({
            "messaging_product": "whatsapp", "recipient_type": "individual",
            "to": to, "type": "text", "text": { "body": text, "preview_url": false },
        });
        if let Some(m) = quoted {
            body["context"] = json!({ "message_id": m });
        }
        let sent = self
            .0
            .conns
            .http()
            .post(format!("{base}/{}/messages", saved.setup.phone_id))
            .bearer_auth(&saved.setup.token)
            .timeout(SEND_WAIT)
            .json(&body)
            .send()
            .await;
        let (status, answer) = match sent {
            Ok(r) => {
                let status = r.status().as_u16();
                (status, r.json::<Value>().await.unwrap_or(Value::Null))
            }
            Err(e) if e.is_connect() || e.is_builder() => {
                return Err(AdapterError::Tool(format!("could not reach WhatsApp: {e}")))
            }
            Err(_) => return Ok(unconfirmed(to)),
        };
        let code = answer["error"]["code"].as_u64();
        match status {
            200..=299 => Ok(ActionOutput::json(json!({
                "sent": true, "id": answer["messages"][0]["id"], "to": to,
            }))),
            408 | 500..=599 => Ok(unconfirmed(to)),
            // Meta's "re-engagement" refusal: the customer service window has closed.
            _ if code == Some(131047) => Err(AdapterError::Tool(window_ended(to))),
            _ if status == 401 || code == Some(190) => Err(AdapterError::Tool(
                "Meta refused the access token; connect WhatsApp Business again".into(),
            )),
            _ => Err(AdapterError::Tool(format!(
                "WhatsApp refused the reply: {}",
                answer["error"]["message"]
                    .as_str()
                    .unwrap_or("no reason given")
            ))),
        }
    }
}

fn window_ended(to: &str) -> String {
    format!(
        "Reply window ended: +{to} wrote more than 24 hours ago, so WhatsApp allows no free-form reply. Send an approved template from WhatsApp Manager, or leave it unsent."
    )
}

fn unconfirmed(to: &str) -> ActionOutput {
    let effect = format!("{REPLY}:{to}:{}", uuid::Uuid::new_v4().simple());
    let says = format!(
        "The WhatsApp reply to +{to} may or may not have been sent. Check the chat in WhatsApp before sending it again."
    );
    ActionOutput::json(json!({
        "status": "submitted_unconfirmed",
        "effect": { "id": effect, "state": "unconfirmed", "retry_allowed": false, "user_message": says },
        "to": to,
    }))
    .with_effect(ExternalEffect {
        id: effect,
        state: ExternalEffectState::Unconfirmed,
        retry_allowed: false,
        user_message: Some(says),
    })
}

/// The evidence card of a WhatsApp result.
pub fn distill(action: &str, out: &Value) -> Option<(String, Vec<String>)> {
    let to = out["to"].as_str().unwrap_or("");
    match action {
        REPLY if out["sent"] == true => Some((format!("replied on WhatsApp to +{to}"), vec![])),
        REPLY => Some((
            format!("the WhatsApp reply to +{to} may not have been sent"),
            vec![],
        )),
        _ => None,
    }
}
