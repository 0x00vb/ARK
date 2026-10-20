//! Verification for one-minute assertions that let a native client pair with one registered
//! runtime. These are not ARK Account API access tokens.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use futures::StreamExt;
use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};
use serde::Deserialize;
use tokio::sync::Mutex;

const ASSERTION_BYTES: usize = 8 * 1024;
const JWKS_BYTES: usize = 1024 * 1024;
const MAX_KEYS: usize = 64;
const MAX_CLOCK_SKEW_SECONDS: u64 = 5;
const ASSERTION_LIFETIME_SECONDS: u64 = 60;
const JWKS_TTL: Duration = Duration::from_secs(60);
const UNKNOWN_KEY_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// The identity carried by a verified and consumed runtime-pair assertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRuntimePairing {
    pub subject: String,
    pub assertion_id: String,
    pub expires_at: u64,
}

/// A verifier used by the runtime listener. Implementations must consume `jti` once.
#[async_trait::async_trait]
pub trait PairingAssertionVerifier: Send + Sync {
    async fn verify(
        &self,
        assertion: &str,
        route_id: &str,
        owner_subject: &str,
    ) -> Result<VerifiedRuntimePairing, PairingVerificationError>;
}

/// Verifies the API's route-specific ES256 pairing assertions against its pairing JWKS.
pub struct RuntimePairingVerifier {
    issuer: String,
    jwks_url: reqwest::Url,
    client: reqwest::Client,
    keys: Mutex<KeyCache>,
    consumed: Mutex<HashMap<String, u64>>,
}

impl RuntimePairingVerifier {
    pub fn new(
        issuer: impl Into<String>,
        jwks_url: impl AsRef<str>,
    ) -> Result<Self, PairingVerificationError> {
        let issuer = issuer.into();
        if issuer.trim() != issuer {
            return Err(PairingVerificationError::Configuration);
        }
        let issuer_url = checked_url(&issuer)?;
        let jwks_url = checked_url(jwks_url.as_ref())?;
        if issuer_url.origin() != jwks_url.origin() {
            return Err(PairingVerificationError::Configuration);
        }
        Ok(Self {
            issuer,
            jwks_url,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| PairingVerificationError::Configuration)?,
            keys: Mutex::new(KeyCache::default()),
            consumed: Mutex::new(HashMap::new()),
        })
    }

    async fn key(&self, kid: &str) -> Result<Vec<u8>, PairingVerificationError> {
        let mut cache = self.keys.lock().await;
        let now = Instant::now();
        let fresh = cache
            .refreshed_at
            .is_some_and(|refreshed| now.duration_since(refreshed) < JWKS_TTL);
        if fresh {
            if let Some(key) = cache.keys.get(kid) {
                return Ok(key.clone());
            }
            if cache.last_unknown_refresh.is_some_and(|refreshed| {
                now.duration_since(refreshed) < UNKNOWN_KEY_REFRESH_INTERVAL
            }) {
                return Err(PairingVerificationError::Key);
            }
        } else if cache
            .last_refresh_completed
            .is_some_and(|completed| now.duration_since(completed) < UNKNOWN_KEY_REFRESH_INTERVAL)
        {
            return Err(if cache.keys.contains_key(kid) {
                PairingVerificationError::Keys
            } else {
                PairingVerificationError::Key
            });
        }
        let had_cached_keys = !cache.keys.is_empty();
        let was_unknown = !cache.keys.contains_key(kid);
        if was_unknown
            && cache.last_unknown_refresh.is_some_and(|refreshed| {
                now.duration_since(refreshed) < UNKNOWN_KEY_REFRESH_INTERVAL
            })
        {
            return Err(PairingVerificationError::Key);
        }
        let fetched = async {
            let response = self
                .client
                .get(self.jwks_url.clone())
                .send()
                .await
                .map_err(|_| PairingVerificationError::Keys)?
                .error_for_status()
                .map_err(|_| PairingVerificationError::Keys)?;
            if response
                .content_length()
                .is_some_and(|length| length > JWKS_BYTES as u64)
            {
                return Err(PairingVerificationError::Keys);
            }
            let mut stream = response.bytes_stream();
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| PairingVerificationError::Keys)?;
                if bytes.len().saturating_add(chunk.len()) > JWKS_BYTES {
                    return Err(PairingVerificationError::Keys);
                }
                bytes.extend_from_slice(&chunk);
            }
            parse_keys(&bytes)
        }
        .await;
        cache.last_refresh_completed = Some(Instant::now());
        if was_unknown {
            cache.last_unknown_refresh = Some(Instant::now());
        }
        cache.keys = fetched?;
        cache.refreshed_at = Some(now);
        let key = cache.keys.get(kid).cloned();
        if key.is_some() && was_unknown && !had_cached_keys {
            cache.last_unknown_refresh = None;
        }
        key.ok_or(PairingVerificationError::Key)
    }

    async fn consume(&self, jti: &str, expires_at: u64) -> Result<(), PairingVerificationError> {
        let now = now_seconds()?;
        let mut consumed = self.consumed.lock().await;
        consumed.retain(|_, expiry| *expiry > now);
        if consumed.contains_key(jti) {
            return Err(PairingVerificationError::Replay);
        }
        consumed.insert(jti.to_string(), expires_at);
        Ok(())
    }
}

#[derive(Default)]
struct KeyCache {
    keys: HashMap<String, Vec<u8>>,
    refreshed_at: Option<Instant>,
    last_refresh_completed: Option<Instant>,
    last_unknown_refresh: Option<Instant>,
}

#[async_trait::async_trait]
impl PairingAssertionVerifier for RuntimePairingVerifier {
    async fn verify(
        &self,
        assertion: &str,
        route_id: &str,
        owner_subject: &str,
    ) -> Result<VerifiedRuntimePairing, PairingVerificationError> {
        if assertion.is_empty() || assertion.len() > ASSERTION_BYTES {
            return Err(PairingVerificationError::Assertion);
        }
        let mut segments = assertion.split('.');
        let (Some(header_segment), Some(claims_segment), Some(signature_segment), None) = (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
        ) else {
            return Err(PairingVerificationError::Assertion);
        };
        let header: Header = decode_json(header_segment)?;
        if header.alg != "ES256"
            || header.typ.as_deref() != Some("ark-runtime-pair+jwt")
            || header.kid.is_empty()
            || header.kid.len() > 200
        {
            return Err(PairingVerificationError::Claims);
        }
        let key = self.key(&header.kid).await?;
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(signature_segment)
            .map_err(|_| PairingVerificationError::Assertion)?;
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, key)
            .verify(
                format!("{header_segment}.{claims_segment}").as_bytes(),
                &signature,
            )
            .map_err(|_| PairingVerificationError::Signature)?;

        let claims: Claims = decode_json(claims_segment)?;
        let now = now_seconds()?;
        let expected_audience = format!("urn:ark:runtime:{route_id}");
        if claims.iss != self.issuer
            || claims.aud != expected_audience
            || claims.sub != owner_subject
            || claims.azp != "ark-ios"
            || claims.scope != "runtime.pair"
            || claims.route_id != route_id
            || claims.jti.is_empty()
            || claims.jti.len() > 200
            || claims.iat > now.saturating_add(MAX_CLOCK_SKEW_SECONDS)
            || claims.iat > claims.exp
            || claims.exp <= now
            || claims.exp.saturating_sub(claims.iat)
                > ASSERTION_LIFETIME_SECONDS + MAX_CLOCK_SKEW_SECONDS
            || claims.exp > now.saturating_add(ASSERTION_LIFETIME_SECONDS + MAX_CLOCK_SKEW_SECONDS)
        {
            return Err(PairingVerificationError::Claims);
        }
        self.consume(&claims.jti, claims.exp).await?;
        Ok(VerifiedRuntimePairing {
            subject: claims.sub,
            assertion_id: claims.jti,
            expires_at: claims.exp,
        })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PairingVerificationError {
    #[error("the runtime pairing verifier is not configured")]
    Configuration,
    #[error("the runtime pairing assertion is malformed")]
    Assertion,
    #[error("the runtime pairing assertion has invalid claims")]
    Claims,
    #[error("the runtime pairing assertion signature is invalid")]
    Signature,
    #[error("the runtime pairing keys are unavailable")]
    Keys,
    #[error("the runtime pairing key is unknown")]
    Key,
    #[error("the runtime pairing assertion was already used")]
    Replay,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    kid: String,
    typ: Option<String>,
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    azp: String,
    scope: String,
    route_id: String,
    jti: String,
    iat: u64,
    exp: u64,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    crv: String,
    kid: String,
    x: String,
    y: String,
    #[serde(default)]
    alg: Option<String>,
    #[serde(rename = "use", default)]
    use_: Option<String>,
}

fn parse_keys(bytes: &[u8]) -> Result<HashMap<String, Vec<u8>>, PairingVerificationError> {
    let jwks: Jwks = serde_json::from_slice(bytes).map_err(|_| PairingVerificationError::Keys)?;
    if jwks.keys.len() > MAX_KEYS {
        return Err(PairingVerificationError::Keys);
    }
    let mut keys = HashMap::new();
    for key in jwks.keys {
        if key.kty != "EC"
            || key.crv != "P-256"
            || key.kid.is_empty()
            || key.kid.len() > 200
            || key.alg.as_deref().is_some_and(|alg| alg != "ES256")
            || key.use_.as_deref().is_some_and(|use_| use_ != "sig")
        {
            continue;
        }
        let x = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(key.x)
            .map_err(|_| PairingVerificationError::Keys)?;
        let y = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(key.y)
            .map_err(|_| PairingVerificationError::Keys)?;
        if x.len() != 32 || y.len() != 32 || keys.contains_key(&key.kid) {
            return Err(PairingVerificationError::Keys);
        }
        let mut public_key = Vec::with_capacity(65);
        public_key.push(4);
        public_key.extend(x);
        public_key.extend(y);
        keys.insert(key.kid, public_key);
    }
    Ok(keys)
}

fn decode_json<T: for<'de> Deserialize<'de>>(segment: &str) -> Result<T, PairingVerificationError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| PairingVerificationError::Assertion)?;
    serde_json::from_slice(&bytes).map_err(|_| PairingVerificationError::Assertion)
}

fn checked_url(value: &str) -> Result<reqwest::Url, PairingVerificationError> {
    let url = reqwest::Url::parse(value).map_err(|_| PairingVerificationError::Configuration)?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(PairingVerificationError::Configuration);
    }
    Ok(url)
}

fn now_seconds() -> Result<u64, PairingVerificationError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| PairingVerificationError::Claims)
}

/// Shared ownership for consumers that keep one verifier across listener restarts.
pub type SharedRuntimePairingVerifier = Arc<RuntimePairingVerifier>;

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    struct SigningKey {
        kid: String,
        key: EcdsaKeyPair,
        jwk: Value,
    }

    impl SigningKey {
        fn new(kid: &str) -> Self {
            let random = SystemRandom::new();
            let document =
                EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &random).unwrap();
            let key = EcdsaKeyPair::from_pkcs8(
                &ECDSA_P256_SHA256_FIXED_SIGNING,
                document.as_ref(),
                &random,
            )
            .unwrap();
            let public = key.public_key().as_ref();
            Self {
                kid: kid.into(),
                jwk: json!({
                    "kty": "EC",
                    "crv": "P-256",
                    "kid": kid,
                    "alg": "ES256",
                    "use": "sig",
                    "x": URL_SAFE_NO_PAD.encode(&public[1..33]),
                    "y": URL_SAFE_NO_PAD.encode(&public[33..65]),
                }),
                key,
            }
        }

        fn assertion(&self, claims: Value) -> String {
            self.assertion_with_header(
                claims,
                json!({
                    "alg": "ES256",
                    "kid": self.kid,
                    "typ": "ark-runtime-pair+jwt"
                }),
            )
        }

        fn assertion_with_header(&self, claims: Value, header: Value) -> String {
            let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
            let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
            let input = format!("{header}.{claims}");
            let signature = self
                .key
                .sign(&SystemRandom::new(), input.as_bytes())
                .unwrap();
            format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
        }
    }

    async fn key_server(
        body: Arc<StdMutex<String>>,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        key_server_with_delay(body, Duration::ZERO).await
    }

    async fn key_server_with_delay(
        body: Arc<StdMutex<String>>,
        response_delay: Duration,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                server_requests.fetch_add(1, Ordering::Relaxed);
                let body = body.lock().unwrap().clone();
                tokio::spawn(async move {
                    let mut request = [0; 2048];
                    let _ = socket.read(&mut request).await;
                    tokio::time::sleep(response_delay).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        (format!("http://{address}"), requests, task)
    }

    fn claims(issuer: &str, route: &str, subject: &str, jti: &str) -> Value {
        let now = now_seconds().unwrap();
        json!({
            "iss": issuer,
            "aud": format!("urn:ark:runtime:{route}"),
            "sub": subject,
            "azp": "ark-ios",
            "scope": "runtime.pair",
            "route_id": route,
            "jti": jti,
            "iat": now,
            "exp": now + 60,
        })
    }

    #[tokio::test]
    async fn exact_claims_unknown_key_refresh_and_replay_are_enforced() {
        let first = SigningKey::new("first");
        let second = SigningKey::new("second");
        let third = SigningKey::new("third");
        let keys = Arc::new(StdMutex::new(
            json!({ "keys": [first.jwk.clone()] }).to_string(),
        ));
        let (issuer, key_requests, server) = key_server(keys.clone()).await;
        let verifier =
            RuntimePairingVerifier::new(&issuer, format!("{issuer}/pairing-jwks")).unwrap();
        let route = "route-macbook";
        let owner = "account-owner";

        let first_assertion = first.assertion(claims(&issuer, route, owner, "jti-first"));
        let verified = verifier
            .verify(&first_assertion, route, owner)
            .await
            .unwrap();
        assert_eq!(verified.subject, owner);
        assert_eq!(verified.assertion_id, "jti-first");
        assert_eq!(
            verifier.verify(&first_assertion, route, owner).await,
            Err(PairingVerificationError::Replay)
        );

        *keys.lock().unwrap() = json!({ "keys": [second.jwk.clone()] }).to_string();
        let second_assertion = second.assertion(claims(&issuer, route, owner, "jti-second"));
        verifier
            .verify(&second_assertion, route, owner)
            .await
            .expect("an unknown kid refreshes the pairing JWKS");
        assert_eq!(key_requests.load(Ordering::Relaxed), 2);

        let third_assertion = third.assertion(claims(&issuer, route, owner, "jti-third"));
        assert_eq!(
            verifier.verify(&third_assertion, route, owner).await,
            Err(PairingVerificationError::Key)
        );
        assert_eq!(
            key_requests.load(Ordering::Relaxed),
            2,
            "novel kids cannot force an unbounded JWKS refresh loop"
        );

        let wrong_route = second.assertion(claims(&issuer, "route-other", owner, "jti-route"));
        assert_eq!(
            verifier.verify(&wrong_route, route, owner).await,
            Err(PairingVerificationError::Claims)
        );
        let mut wrong_client_claims = claims(&issuer, route, owner, "jti-client");
        wrong_client_claims["azp"] = json!("ark-desktop");
        let wrong_client = second.assertion(wrong_client_claims);
        assert_eq!(
            verifier.verify(&wrong_client, route, owner).await,
            Err(PairingVerificationError::Claims)
        );

        let mut wrong_issuer_claims = claims(&issuer, route, owner, "jti-issuer");
        wrong_issuer_claims["iss"] = json!("https://other.example");
        let mut wrong_audience_claims = claims(&issuer, route, owner, "jti-audience");
        wrong_audience_claims["aud"] = json!("urn:ark:runtime:other");
        let mut wrong_subject_claims = claims(&issuer, route, owner, "jti-subject");
        wrong_subject_claims["sub"] = json!("another-owner");
        let mut wrong_scope_claims = claims(&issuer, route, owner, "jti-scope");
        wrong_scope_claims["scope"] = json!("openid runtime.pair");
        let now = now_seconds().unwrap();
        let mut expired_claims = claims(&issuer, route, owner, "jti-expired");
        expired_claims["iat"] = json!(now - 60);
        expired_claims["exp"] = json!(now);
        let mut long_claims = claims(&issuer, route, owner, "jti-long");
        long_claims["exp"] = json!(now + ASSERTION_LIFETIME_SECONDS + MAX_CLOCK_SKEW_SECONDS + 1);
        for invalid in [
            wrong_issuer_claims,
            wrong_audience_claims,
            wrong_subject_claims,
            wrong_scope_claims,
            expired_claims,
            long_claims,
        ] {
            let assertion = second.assertion(invalid);
            assert_eq!(
                verifier.verify(&assertion, route, owner).await,
                Err(PairingVerificationError::Claims)
            );
        }

        let wrong_type = second.assertion_with_header(
            claims(&issuer, route, owner, "jti-type"),
            json!({ "alg": "ES256", "kid": "second", "typ": "JWT" }),
        );
        assert_eq!(
            verifier.verify(&wrong_type, route, owner).await,
            Err(PairingVerificationError::Claims)
        );
        let mut tampered = second.assertion(claims(&issuer, route, owner, "jti-signature"));
        tampered.push('A');
        assert_eq!(
            verifier.verify(&tampered, route, owner).await,
            Err(PairingVerificationError::Signature)
        );
        server.abort();
    }

    #[tokio::test]
    async fn an_empty_key_set_cannot_force_repeated_refreshes() {
        let signing_key = SigningKey::new("missing");
        let keys = Arc::new(StdMutex::new(json!({ "keys": [] }).to_string()));
        let (issuer, key_requests, server) = key_server(keys).await;
        let verifier =
            RuntimePairingVerifier::new(&issuer, format!("{issuer}/pairing-jwks")).unwrap();
        let assertion = signing_key.assertion(claims(
            &issuer,
            "route-macbook",
            "account-owner",
            "jti-missing",
        ));

        for _ in 0..2 {
            assert_eq!(
                verifier
                    .verify(&assertion, "route-macbook", "account-owner")
                    .await,
                Err(PairingVerificationError::Key)
            );
        }
        assert_eq!(
            key_requests.load(Ordering::Relaxed),
            1,
            "an empty key set must still throttle unknown-key refreshes"
        );
        server.abort();
    }

    #[tokio::test]
    async fn unknown_key_throttle_starts_when_refresh_finishes() {
        let signing_key = SigningKey::new("missing");
        let keys = Arc::new(StdMutex::new(json!({ "keys": [] }).to_string()));
        let (issuer, _, server) = key_server_with_delay(keys, Duration::from_millis(50)).await;
        let verifier =
            RuntimePairingVerifier::new(&issuer, format!("{issuer}/pairing-jwks")).unwrap();
        let assertion = signing_key.assertion(claims(
            &issuer,
            "route-macbook",
            "account-owner",
            "jti-missing",
        ));

        assert_eq!(
            verifier
                .verify(&assertion, "route-macbook", "account-owner")
                .await,
            Err(PairingVerificationError::Key)
        );
        let cache = verifier.keys.lock().await;
        assert!(cache
            .last_unknown_refresh
            .is_some_and(|finished| finished.elapsed() < Duration::from_millis(25)));
        server.abort();
    }

    #[tokio::test]
    async fn a_failed_expired_key_refresh_is_throttled() {
        let signing_key = SigningKey::new("known");
        let keys = Arc::new(StdMutex::new(
            json!({ "keys": [signing_key.jwk.clone()] }).to_string(),
        ));
        let (issuer, key_requests, server) = key_server(keys.clone()).await;
        let verifier =
            RuntimePairingVerifier::new(&issuer, format!("{issuer}/pairing-jwks")).unwrap();
        let assertion = signing_key.assertion(claims(
            &issuer,
            "route-macbook",
            "account-owner",
            "jti-known",
        ));
        verifier.key("known").await.unwrap();
        {
            let mut cache = verifier.keys.lock().await;
            cache.refreshed_at = Some(Instant::now() - JWKS_TTL);
            cache.last_refresh_completed = Some(Instant::now() - JWKS_TTL);
        }
        *keys.lock().unwrap() = "not-json".into();

        for _ in 0..2 {
            assert_eq!(
                verifier.key("known").await,
                Err(PairingVerificationError::Keys)
            );
        }
        assert_eq!(
            key_requests.load(Ordering::Relaxed),
            2,
            "a failed refresh of a known expired key must be throttled"
        );
        assert_eq!(
            verifier
                .verify(&assertion, "route-macbook", "account-owner")
                .await,
            Err(PairingVerificationError::Keys)
        );
        server.abort();
    }

    #[test]
    fn verifier_configuration_is_https_and_same_origin() {
        assert!(RuntimePairingVerifier::new(
            "https://api.aiark.dev",
            "https://api.aiark.dev/.well-known/runtime-pairing-jwks.json"
        )
        .is_ok());
        assert_eq!(
            RuntimePairingVerifier::new(
                "https://api.aiark.dev",
                "https://keys.aiark.dev/runtime-pairing.json"
            )
            .err(),
            Some(PairingVerificationError::Configuration)
        );
        assert_eq!(
            RuntimePairingVerifier::new(
                "http://api.aiark.dev",
                "http://api.aiark.dev/runtime-pairing.json"
            )
            .err(),
            Some(PairingVerificationError::Configuration)
        );
        assert_eq!(
            RuntimePairingVerifier::new(
                "https://api.aiark.dev",
                "https://api.aiark.dev/runtime-pairing.json?redirect=1"
            )
            .err(),
            Some(PairingVerificationError::Configuration)
        );
    }
}
