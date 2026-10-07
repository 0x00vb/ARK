//! Notifications to a person's devices (ADR 0047): Web Push from the runtime itself, with no
//! service of ARK's in between. The runtime keeps a key pair (VAPID, RFC 8292) and each device's
//! subscription; a notification is encrypted to the device (RFC 8291, `aes128gcm`) and sent to
//! the device's push service, which sees when, never what.
//!
//! ```text
//! page: PushManager.subscribe(runtime's public key) -> push_add {endpoint, p256dh, auth}
//! runtime: something waits -> seal(payload, device keys) -> POST endpoint (VAPID JWT)
//! push service -> the device's service worker (sw.js) -> a notification -> a tap opens the item
//! ```

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use ring::rand::SecureRandom;
use ring::{aead, agreement, hmac, rand, signature};
use serde::{Deserialize, Serialize};

/// The record kind of a device that asked for notifications.
pub const DEVICE: &str = "push_device";
/// At most this many devices per runtime: an endpoint is a URL a client gave, and every one
/// gets a request from the runtime.
pub const MAX_DEVICES: usize = 10;
/// The record size of a sealed message (one record: a notification is small).
const RS: u32 = 4096;

/// A device that asked for notifications.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Device {
    /// Where its push service takes messages for it.
    pub endpoint: String,
    /// Its public key (P-256, uncompressed) and auth secret, base64url.
    pub p256dh: String,
    pub auth: String,
    /// What the person calls it ("iPhone Safari").
    pub name: String,
    pub added_ms: u64,
    /// The token or session name it was added with: revoking that name stops its notifications.
    pub by: String,
    /// The runtime's address as the device saw it: the contact a push service is given.
    pub origin: String,
}

impl Device {
    /// The record id of the device: its endpoint's digest.
    pub fn id(endpoint: &str) -> String {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(endpoint.as_bytes());
        d.iter().take(16).map(|b| format!("{b:02x}")).collect()
    }
}

/// The push services ARK sends to. Any other endpoint is refused: a signed-in client must not
/// make the runtime send requests where it likes.
pub fn known_service(endpoint: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    let host = url.host_str().unwrap_or("");
    host == "fcm.googleapis.com"
        || host == "updates.push.services.mozilla.com"
        || host == "push.services.mozilla.com"
        || host.ends_with(".push.apple.com")
        || host.ends_with(".notify.windows.com")
}

/// The runtime's key pair, made once and kept in the store (0600). It only signs messages to
/// devices whose own keys are in the same store.
pub struct Keys {
    pair: signature::EcdsaKeyPair,
}

impl Keys {
    /// The keys kept at `path`, made there on first use.
    pub fn in_file(path: &Path) -> anyhow::Result<Keys> {
        let rng = rand::SystemRandom::new();
        let pkcs8 = match std::fs::read_to_string(path) {
            Ok(s) => B64.decode(s.trim())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let doc = signature::EcdsaKeyPair::generate_pkcs8(
                    &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                    &rng,
                )
                .map_err(|_| anyhow::anyhow!("could not make a key"))?;
                write_private(path, &B64.encode(doc.as_ref()))?;
                doc.as_ref().to_vec()
            }
            Err(e) => return Err(e.into()),
        };
        let pair = signature::EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &pkcs8,
            &rng,
        )
        .map_err(|_| anyhow::anyhow!("the push key in {} is not usable", path.display()))?;
        Ok(Keys { pair })
    }

    /// The public key, base64url: the page subscribes with it.
    pub fn public(&self) -> String {
        B64.encode(signature::KeyPair::public_key(&self.pair).as_ref())
    }

    /// `Authorization` for a request to `endpoint` (RFC 8292): a JWT for the endpoint's origin,
    /// good for 12 hours, naming `contact`.
    pub fn authorization(
        &self,
        endpoint: &str,
        contact: &str,
        now_s: u64,
    ) -> anyhow::Result<String> {
        let url = reqwest::Url::parse(endpoint)?;
        let aud = url.origin().ascii_serialization();
        let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64.encode(
            serde_json::json!({ "aud": aud, "exp": now_s + 12 * 3600, "sub": contact }).to_string(),
        );
        let input = format!("{header}.{claims}");
        let sig = self
            .pair
            .sign(&rand::SystemRandom::new(), input.as_bytes())
            .map_err(|_| anyhow::anyhow!("could not sign"))?;
        Ok(format!(
            "vapid t={input}.{}, k={}",
            B64.encode(sig.as_ref()),
            self.public()
        ))
    }
}

fn write_private(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    use std::io::Write;
    o.open(path)?.write_all(text.as_bytes())?;
    Ok(())
}

/// Seal `plaintext` for a device (RFC 8291): a fresh key pair and salt per message.
pub fn seal(p256dh: &str, auth: &str, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    let ua_public = B64.decode(p256dh.trim_end_matches('='))?;
    let auth = B64.decode(auth.trim_end_matches('='))?;
    let rng = rand::SystemRandom::new();
    let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
        .map_err(|_| anyhow::anyhow!("no key"))?;
    let as_public = private
        .compute_public_key()
        .map_err(|_| anyhow::anyhow!("no key"))?
        .as_ref()
        .to_vec();
    let secret = agreement::agree_ephemeral(
        private,
        &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &ua_public),
        |s| s.to_vec(),
    )
    .map_err(|_| anyhow::anyhow!("the device's key is not a P-256 key"))?;
    let mut salt = [0u8; 16];
    rng.fill(&mut salt)
        .map_err(|_| anyhow::anyhow!("no salt"))?;
    seal_with(&secret, &auth, &ua_public, &as_public, &salt, plaintext)
}

/// The content key and nonce of RFC 8291 § 3.4 from the shared secret.
fn derive(
    ecdh: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8],
) -> ([u8; 16], [u8; 12]) {
    let mac = |key: &[u8], parts: &[&[u8]]| {
        let k = hmac::Key::new(hmac::HMAC_SHA256, key);
        let mut c = hmac::Context::with_key(&k);
        for p in parts {
            c.update(p);
        }
        c.sign()
    };
    let prk_key = mac(auth, &[ecdh]);
    let ikm = mac(
        prk_key.as_ref(),
        &[b"WebPush: info\0", ua_public, as_public, &[1]],
    );
    let prk = mac(salt, &[ikm.as_ref()]);
    let cek = mac(prk.as_ref(), &[b"Content-Encoding: aes128gcm\0", &[1]]);
    let nonce = mac(prk.as_ref(), &[b"Content-Encoding: nonce\0", &[1]]);
    let mut k = [0u8; 16];
    k.copy_from_slice(&cek.as_ref()[..16]);
    let mut n = [0u8; 12];
    n.copy_from_slice(&nonce.as_ref()[..12]);
    (k, n)
}

/// The `aes128gcm` body: salt, record size, the sender's key, one record.
fn seal_with(
    ecdh: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8; 16],
    plaintext: &[u8],
) -> anyhow::Result<Vec<u8>> {
    if plaintext.len() + 17 + 86 > RS as usize {
        anyhow::bail!("a notification is too long");
    }
    let (cek, nonce) = derive(ecdh, auth, ua_public, as_public, salt);
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_128_GCM, &cek).map_err(|_| anyhow::anyhow!("key"))?,
    );
    let mut record = plaintext.to_vec();
    // The last (and only) record's delimiter.
    record.push(2);
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::empty(),
        &mut record,
    )
    .map_err(|_| anyhow::anyhow!("could not seal"))?;
    let mut body = Vec::with_capacity(16 + 5 + as_public.len() + record.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RS.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&record);
    Ok(body)
}

/// How sending to one device went.
#[derive(Debug, PartialEq)]
pub enum Sent {
    Ok,
    /// The device is gone (404, 410): forget it.
    Gone,
    /// Try later (429, 5xx), or the network failed.
    Later(String),
    /// Refused for good (an unknown service, a bad key).
    Refused(String),
}

/// What one notification says. The words come from `ark_interact::attention::notice`.
#[derive(Clone, Debug, Serialize)]
pub struct Message {
    /// The item it is about: a tap opens it.
    pub id: String,
    pub title: String,
    pub body: String,
    /// Notifications with one tag replace each other on the device.
    pub tag: String,
    /// A question: the push service delivers it first.
    #[serde(skip)]
    pub urgent: bool,
}

/// Send `m` to `d`.
pub async fn send(client: &reqwest::Client, keys: &Keys, d: &Device, m: &Message) -> Sent {
    if !known_service(&d.endpoint) {
        return Sent::Refused("not a known push service".into());
    }
    let body = match serde_json::to_vec(m)
        .map_err(anyhow::Error::from)
        .and_then(|p| seal(&d.p256dh, &d.auth, &p))
    {
        Ok(b) => b,
        Err(e) => return Sent::Refused(e.to_string()),
    };
    let now_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let contact = if d.origin.starts_with("https://") {
        d.origin.clone()
    } else {
        "mailto:ark@localhost".into()
    };
    let auth = match keys.authorization(&d.endpoint, &contact, now_s) {
        Ok(a) => a,
        Err(e) => return Sent::Refused(e.to_string()),
    };
    let res = client
        .post(&d.endpoint)
        .header("Authorization", auth)
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("TTL", "86400")
        .header("Urgency", if m.urgent { "high" } else { "normal" })
        // Messages waiting for an offline device collapse to the newest per item.
        .header("Topic", topic(&m.id))
        .body(body)
        .send()
        .await;
    match res {
        Ok(r) if r.status().is_success() => Sent::Ok,
        Ok(r) if matches!(r.status().as_u16(), 404 | 410) => Sent::Gone,
        Ok(r) if r.status().as_u16() == 429 || r.status().is_server_error() => {
            Sent::Later(format!("the push service answered {}", r.status()))
        }
        Ok(r) => Sent::Refused(format!("the push service answered {}", r.status())),
        Err(e) => Sent::Later(e.to_string()),
    }
}

/// A `Topic` header (at most 32 base64url characters) for an item.
fn topic(id: &str) -> String {
    use sha2::{Digest, Sha256};
    B64.encode(&Sha256::digest(id.as_bytes())[..24])
}

/// Where the runtime's key is kept in a store.
pub fn key_file(store: &Path) -> PathBuf {
    store.join("push-key")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Vec<u8> {
        B64.decode(s).unwrap()
    }

    /// RFC 8291 Appendix A: the same body from the same keys, secret and salt.
    #[test]
    fn the_rfc_example_seals_to_the_rfc_body() {
        let ua_public = d("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let as_public = d("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
        let auth = d("BTBZMqHH6r4Tts7J_aSIgg");
        let ecdh = d("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs");
        let salt: [u8; 16] = d("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let (cek, nonce) = derive(&ecdh, &auth, &ua_public, &as_public, &salt);
        assert_eq!(B64.encode(cek), "oIhVW04MRdy2XN9CiKLxTg");
        assert_eq!(B64.encode(nonce), "4h_95klXJ5E_qnoN");
        let body = seal_with(
            &ecdh,
            &auth,
            &ua_public,
            &as_public,
            &salt,
            b"When I grow up, I want to be a watermelon",
        )
        .unwrap();
        assert_eq!(
            B64.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    /// A device opens what the runtime sealed for it.
    #[test]
    fn a_device_opens_what_was_sealed_for_it() {
        let rng = rand::SystemRandom::new();
        let ua = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let ua_public = ua.compute_public_key().unwrap().as_ref().to_vec();
        let mut auth = [0u8; 16];
        rng.fill(&mut auth).unwrap();
        let body = seal(&B64.encode(&ua_public), &B64.encode(auth), b"hello").unwrap();
        let salt = &body[..16];
        assert_eq!(&body[16..20], &RS.to_be_bytes());
        let n = body[20] as usize;
        let as_public = &body[21..21 + n];
        let ecdh = agreement::agree_ephemeral(
            ua,
            &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public),
            |s| s.to_vec(),
        )
        .unwrap();
        let (cek, nonce) = derive(&ecdh, &auth, &ua_public, as_public, salt);
        let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
        let mut record = body[21 + n..].to_vec();
        let plain = key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::empty(),
                &mut record,
            )
            .unwrap();
        assert_eq!(plain, b"hello\x02");
    }

    /// The JWT verifies with the public key the page subscribed with, and names the endpoint's
    /// origin.
    #[test]
    fn the_authorization_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let keys = Keys::in_file(&dir.path().join("k")).unwrap();
        let again = Keys::in_file(&dir.path().join("k")).unwrap();
        assert_eq!(keys.public(), again.public(), "kept, not made again");
        let a = keys
            .authorization(
                "https://fcm.googleapis.com/fcm/send/abc",
                "https://ark.example.com",
                1000,
            )
            .unwrap();
        let t = a.strip_prefix("vapid t=").unwrap();
        let (jwt, k) = t.split_once(", k=").unwrap();
        assert_eq!(k, keys.public());
        let (input, sig) = jwt.rsplit_once('.').unwrap();
        signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, d(k))
            .verify(input.as_bytes(), &d(sig))
            .unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&d(input.split_once('.').unwrap().1)).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["sub"], "https://ark.example.com");
        assert_eq!(claims["exp"], 1000 + 12 * 3600);
    }

    #[test]
    fn only_known_push_services() {
        assert!(known_service("https://fcm.googleapis.com/fcm/send/x"));
        assert!(known_service("https://web.push.apple.com/abc"));
        assert!(known_service(
            "https://updates.push.services.mozilla.com/wpush/v2/x"
        ));
        assert!(!known_service("http://fcm.googleapis.com/x"));
        assert!(!known_service("https://evil.example.com/push.apple.com"));
        assert!(!known_service("https://127.0.0.1/x"));
    }
}
