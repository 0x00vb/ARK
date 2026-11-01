use std::collections::BTreeMap;

use ark_interact::ark_orchestrate::automation::InputValue;
use ark_sdk::prelude::ark_tools::events::{
    Direction, ExternalEvent, HookError, Limits, WebhookSource,
};
use base64::Engine as _;
use ring::hmac;
use serde::{Deserialize, Serialize};

pub(crate) const MAX_BODY: usize = 64 * 1024;
const MAX_AGE_MS: u64 = 300_000;
const VERSION: &str = "ark-webhook-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Deliver,
    Preview,
}
impl Mode {
    fn word(self) -> &'static str {
        match self {
            Self::Deliver => "deliver",
            Self::Preview => "preview",
        }
    }
}

pub(crate) struct Authentication<'a> {
    pub endpoint: &'a str,
    pub connection: &'a str,
    pub secret: &'a str,
}

pub(crate) struct Verified {
    pub event: String,
    pub at_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Inputs {
    pub inputs: BTreeMap<String, InputValue>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Challenge {
    pub challenge: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChallengeReply {
    pub endpoint: String,
    pub connection: String,
    pub challenge: String,
    pub signature: String,
}

fn unverified() -> HookError {
    HookError::Unverified("request is not verified".into())
}
fn identity(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}
fn bytes(
    auth: &Authentication<'_>,
    mode: Mode,
    timestamp: &str,
    event: &str,
    body: &[u8],
) -> Vec<u8> {
    let mut bytes = format!(
        "{VERSION}\n{}\n{}\n{}\n{timestamp}\n{event}\n",
        auth.endpoint,
        auth.connection,
        mode.word()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}
fn encode(signature: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature)
}
fn decode(signature: &str) -> Result<Vec<u8>, HookError> {
    if signature.len() != 43 {
        return Err(unverified());
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| unverified())?;
    if bytes.len() != 32 {
        return Err(unverified());
    }
    Ok(bytes)
}

pub(crate) fn signed_headers(
    auth: &Authentication<'_>,
    mode: Mode,
    timestamp: u64,
    event: &str,
    body: &[u8],
) -> BTreeMap<String, String> {
    let timestamp = timestamp.to_string();
    let key = hmac::Key::new(hmac::HMAC_SHA256, auth.secret.as_bytes());
    let signature = hmac::sign(&key, &bytes(auth, mode, &timestamp, event, body));
    BTreeMap::from([
        ("x-ark-connection".into(), auth.connection.into()),
        ("x-ark-mode".into(), mode.word().into()),
        ("x-ark-timestamp".into(), timestamp),
        ("x-ark-event".into(), event.into()),
        (
            "x-ark-signature".into(),
            format!("v1={}", encode(signature.as_ref())),
        ),
    ])
}

pub(crate) fn authenticate(
    auth: &Authentication<'_>,
    headers: &BTreeMap<String, String>,
    body: &[u8],
    mode: Mode,
    now: u64,
) -> Result<Verified, HookError> {
    let header = |name| headers.get(name).map(String::as_str).ok_or_else(unverified);
    if body.len() > MAX_BODY
        || !identity(auth.endpoint, 128)
        || !identity(auth.connection, 128)
        || header("x-ark-connection")? != auth.connection
        || header("x-ark-mode")? != mode.word()
    {
        return Err(unverified());
    }
    let timestamp = header("x-ark-timestamp")?;
    let at_ms = timestamp.parse::<u64>().map_err(|_| unverified())?;
    if timestamp != at_ms.to_string() || now.abs_diff(at_ms) > MAX_AGE_MS {
        return Err(unverified());
    }
    let event = header("x-ark-event")?;
    if !identity(event, 128) {
        return Err(unverified());
    }
    let signature = decode(
        header("x-ark-signature")?
            .strip_prefix("v1=")
            .ok_or_else(unverified)?,
    )?;
    let key = hmac::Key::new(hmac::HMAC_SHA256, auth.secret.as_bytes());
    hmac::verify(&key, &bytes(auth, mode, timestamp, event, body), &signature)
        .map_err(|_| unverified())?;
    Ok(Verified {
        event: event.into(),
        at_ms,
    })
}

fn response_bytes(auth: &Authentication<'_>, event: &str, challenge: &str) -> Vec<u8> {
    format!(
        "ark-webhook-preview-response-v1\n{}\n{}\n{event}\n{challenge}\n",
        auth.endpoint, auth.connection
    )
    .into_bytes()
}
pub(crate) fn preview_reply(
    auth: &Authentication<'_>,
    event: &str,
    challenge: &str,
) -> Result<ChallengeReply, HookError> {
    if !identity(event, 128) || !identity(challenge, 128) {
        return Err(HookError::Malformed(
            "the preview challenge is invalid".into(),
        ));
    }
    let key = hmac::Key::new(hmac::HMAC_SHA256, auth.secret.as_bytes());
    let signature = hmac::sign(&key, &response_bytes(auth, event, challenge));
    Ok(ChallengeReply {
        endpoint: auth.endpoint.into(),
        connection: auth.connection.into(),
        challenge: challenge.into(),
        signature: encode(signature.as_ref()),
    })
}
pub(crate) fn verify_preview_reply(
    auth: &Authentication<'_>,
    event: &str,
    challenge: &str,
    reply: &ChallengeReply,
) -> Result<(), HookError> {
    if reply.endpoint != auth.endpoint
        || reply.connection != auth.connection
        || reply.challenge != challenge
    {
        return Err(unverified());
    }
    let signature = decode(&reply.signature)?;
    let key = hmac::Key::new(hmac::HMAC_SHA256, auth.secret.as_bytes());
    hmac::verify(&key, &response_bytes(auth, event, challenge), &signature)
        .map_err(|_| unverified())
}

pub(crate) struct Source<'a> {
    pub auth: Authentication<'a>,
    pub account: &'a str,
    pub automation: &'a str,
    pub now: u64,
}
impl WebhookSource for Source<'_> {
    fn source(&self) -> &str {
        "webhook"
    }
    fn account(&self) -> &str {
        self.account
    }
    fn challenge(&self, _: &BTreeMap<String, String>) -> Result<String, HookError> {
        Err(HookError::Malformed("use a signed POST preview".into()))
    }
    fn decode(
        &self,
        headers: &BTreeMap<String, String>,
        raw: &[u8],
    ) -> Result<Vec<ExternalEvent>, HookError> {
        let verified = authenticate(&self.auth, headers, raw, Mode::Deliver, self.now)?;
        let inputs: Inputs = serde_json::from_slice(raw)
            .map_err(|_| HookError::Malformed("the webhook inputs are invalid".into()))?;
        let event = ExternalEvent {
            source: self.source().into(),
            account: self.account.into(),
            key: verified.event,
            kind: "webhook".into(),
            direction: Direction::Inbound,
            actor: None,
            subject: Some(self.automation.into()),
            at_ms: verified.at_ms,
            label: "Webhook event".into(),
            fields: Default::default(),
            payload: Some(
                serde_json::to_string(&inputs)
                    .map_err(|_| HookError::Malformed("the webhook inputs are invalid".into()))?,
            ),
        };
        event
            .check(&Limits {
                raw_body: MAX_BODY,
                payload: MAX_BODY,
                ..Default::default()
            })
            .map_err(|_| HookError::Malformed("the webhook event exceeds its limits".into()))?;
        Ok(vec![event])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn authentication() -> Authentication<'static> {
        Authentication {
            endpoint: "wh-fixed-endpoint",
            connection: "hc-fixed-connection",
            secret: "test-only-generated-secret",
        }
    }
    #[test]
    fn signed_bytes_bind_body_mode_route_connection_event_and_time_before_decoding() {
        let auth = authentication();
        let body = br#"{"inputs":{"text":{"kind":"text","value":"hello"}}}"#;
        let headers = signed_headers(&auth, Mode::Deliver, 1_000_000, "event-one", body);
        let source = Source {
            auth: authentication(),
            account: "trusted-account",
            automation: "fixed-definition",
            now: 1_000_000,
        };
        let events = source.decode(&headers, body).unwrap();
        assert_eq!(events[0].key, "event-one");
        assert_eq!(events[0].account, "trusted-account");
        assert!(source.decode(&headers, br#"{"inputs":{}}"#).is_err());
        for (name, value) in [
            ("x-ark-mode", "preview"),
            ("x-ark-connection", "different"),
            ("x-ark-event", "different"),
            ("x-ark-timestamp", "1000001"),
        ] {
            let mut changed = headers.clone();
            changed.insert(name.into(), value.into());
            assert!(source.decode(&changed, body).is_err());
        }
        let other = Authentication {
            endpoint: "different-endpoint",
            ..authentication()
        };
        assert!(authenticate(&other, &headers, body, Mode::Deliver, 1_000_000).is_err());
        assert!(authenticate(&auth, &headers, body, Mode::Deliver, 1_300_001).is_err());
        assert!(authenticate(&auth, &headers, body, Mode::Deliver, 699_999).is_err());
        let malformed = b"not JSON";
        assert!(matches!(
            source.decode(&headers, malformed),
            Err(HookError::Unverified(_))
        ));
        let signed_bad = signed_headers(&auth, Mode::Deliver, 1_000_000, "bad-json", malformed);
        assert!(matches!(
            source.decode(&signed_bad, malformed),
            Err(HookError::Malformed(_))
        ));
    }
    #[test]
    fn preview_is_a_separate_authenticated_exchange_and_an_echo_is_not_proof() {
        let auth = authentication();
        let body = br#"{"challenge":"unique-proof"}"#;
        let headers = signed_headers(&auth, Mode::Preview, 1_000_000, "probe-one", body);
        assert!(authenticate(&auth, &headers, body, Mode::Deliver, 1_000_000).is_err());
        authenticate(&auth, &headers, body, Mode::Preview, 1_000_000).unwrap();
        let reply = preview_reply(&auth, "probe-one", "unique-proof").unwrap();
        verify_preview_reply(&auth, "probe-one", "unique-proof", &reply).unwrap();
        assert!(verify_preview_reply(&auth, "probe-two", "unique-proof", &reply).is_err());
        let echo = ChallengeReply {
            signature: reply.challenge.clone(),
            ..reply
        };
        assert!(verify_preview_reply(&auth, "probe-one", "unique-proof", &echo).is_err());
    }
    #[test]
    fn a_signed_body_cannot_supply_routing_or_use_an_ambiguous_identity() {
        let auth = authentication();
        let now = 1_000_000;
        let source = Source {
            auth: authentication(),
            account: "trusted-account",
            automation: "fixed-definition",
            now,
        };
        let body = br#"{"inputs":{},"project":"foreign","account":"foreign"}"#;
        let headers = signed_headers(&auth, Mode::Deliver, now, "event-one", body);
        assert!(matches!(
            source.decode(&headers, body),
            Err(HookError::Malformed(_))
        ));
        let bad = signed_headers(&auth, Mode::Deliver, now, "event\nsecond-field", body);
        assert!(authenticate(&auth, &bad, body, Mode::Deliver, now).is_err());
        let oversized = vec![b'x'; MAX_BODY + 1];
        let headers = signed_headers(&auth, Mode::Deliver, now, "too-large", &oversized);
        assert!(authenticate(&auth, &headers, &oversized, Mode::Deliver, now).is_err());
    }
}
