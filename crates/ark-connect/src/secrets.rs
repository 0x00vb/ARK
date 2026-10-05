//! Where account tokens and API keys are kept: the OS keychain (macOS Keychain, Windows
//! Credential Manager, the Linux kernel keyring) on a person's computer, or [`FileSecrets`], one
//! encrypted file in the store, on a server, where a kernel keyring does not survive a restart
//! (ADR 0028). Tests bring [`MemorySecrets`].

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};

use crate::ConnectError;

pub trait Secrets: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, ConnectError>;
    fn set(&self, key: &str, value: &str) -> Result<(), ConnectError>;
    fn delete(&self, key: &str) -> Result<(), ConnectError>;
}

/// The OS keychain, under the service name `ark`.
pub struct Keychain;

const SERVICE: &str = "ark";

fn keychain_error(e: keyring::Error) -> ConnectError {
    ConnectError::Secrets(e.to_string())
}

impl Secrets for Keychain {
    fn get(&self, key: &str) -> Result<Option<String>, ConnectError> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(keychain_error)?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keychain_error(e)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), ConnectError> {
        keyring::Entry::new(SERVICE, key)
            .and_then(|e| e.set_password(value))
            .map_err(keychain_error)
    }

    fn delete(&self, key: &str) -> Result<(), ConnectError> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(keychain_error)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keychain_error(e)),
        }
    }
}

/// Secrets in memory, gone when the process ends (tests).
#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<String, String>>);

impl Secrets for MemorySecrets {
    fn get(&self, key: &str) -> Result<Option<String>, ConnectError> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), ConnectError> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), ConnectError> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

/// Secrets in one file, `<store>/secrets.enc`, encrypted with AES-256-GCM. The key is 32 bytes
/// from `ARK_SECRETS_KEY` (base64) when the host sets it, so it can stay off the disk the
/// store is on, or else `<store>/secrets.key`, made on first use. Both files are mode 0600 and
/// written whole, through a temporary file. No error says what a secret is.
pub struct FileSecrets {
    path: PathBuf,
    key: LessSafeKey,
    lock: Mutex<()>,
}

/// The variable a host sets to keep the key out of the store: 32 bytes, base64.
pub const SECRETS_KEY_VAR: &str = "ARK_SECRETS_KEY";

const SECRETS_FILE: &str = "secrets.enc";
const KEY_FILE: &str = "secrets.key";
/// Bound into every encryption, so the file cannot be passed off as another format.
const AAD: &[u8] = b"ark-secrets-v1";

#[derive(serde::Serialize, serde::Deserialize)]
struct Sealed {
    version: u32,
    nonce: String,
    data: String,
}

fn secrets_error(msg: impl Into<String>) -> ConnectError {
    ConnectError::Secrets(msg.into())
}

impl FileSecrets {
    /// The store's secrets: `<root>/secrets.enc`, with the key from `ARK_SECRETS_KEY` or
    /// `<root>/secrets.key`. Refuses when secrets exist and their key does not.
    pub fn in_store(root: &Path) -> Result<FileSecrets, ConnectError> {
        let path = root.join(SECRETS_FILE);
        let key = match std::env::var(SECRETS_KEY_VAR) {
            Ok(v) => decode_key(v.trim()).ok_or_else(|| {
                secrets_error(format!(
                    "{SECRETS_KEY_VAR} must be 32 bytes in base64 (openssl rand -base64 32)"
                ))
            })?,
            Err(_) => key_file(root, path.exists())?,
        };
        let s = FileSecrets::with_key(path, key);
        // Read once now, so a wrong key stops the app at start, not at the first sign-in.
        s.read()?;
        Ok(s)
    }

    /// Secrets at `path` with this key.
    pub fn with_key(path: impl Into<PathBuf>, key: [u8; 32]) -> FileSecrets {
        let unbound = UnboundKey::new(&AES_256_GCM, &key).expect("a 32-byte key");
        FileSecrets {
            path: path.into(),
            key: LessSafeKey::new(unbound),
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<BTreeMap<String, String>, ConnectError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e.into()),
        };
        let unreadable = || {
            secrets_error(format!(
                "{} cannot be read with this key (a different {SECRETS_KEY_VAR} or {KEY_FILE}?)",
                self.path.display()
            ))
        };
        let sealed: Sealed = serde_json::from_slice(&bytes).map_err(|_| unreadable())?;
        if sealed.version != 1 {
            return Err(secrets_error(format!(
                "{} is version {}; this ARK reads version 1",
                self.path.display(),
                sealed.version
            )));
        }
        let nonce: [u8; NONCE_LEN] = B64
            .decode(&sealed.nonce)
            .ok()
            .and_then(|n| n.try_into().ok())
            .ok_or_else(unreadable)?;
        let mut data = B64.decode(&sealed.data).map_err(|_| unreadable())?;
        let plain = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(AAD),
                &mut data,
            )
            .map_err(|_| unreadable())?;
        serde_json::from_slice(plain).map_err(|_| unreadable())
    }

    fn write(&self, map: &BTreeMap<String, String>) -> Result<(), ConnectError> {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| secrets_error("no randomness for a nonce"))?;
        let mut data = serde_json::to_vec(map).map_err(|e| secrets_error(e.to_string()))?;
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(AAD),
                &mut data,
            )
            .map_err(|_| secrets_error("could not encrypt"))?;
        let sealed = Sealed {
            version: 1,
            nonce: B64.encode(nonce),
            data: B64.encode(&data),
        };
        let body = serde_json::to_vec(&sealed).map_err(|e| secrets_error(e.to_string()))?;
        write_private(&self.path, &body)
    }

    fn change(&self, f: impl FnOnce(&mut BTreeMap<String, String>)) -> Result<(), ConnectError> {
        let _held = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut map = self.read()?;
        f(&mut map);
        self.write(&map)
    }
}

impl Secrets for FileSecrets {
    fn get(&self, key: &str) -> Result<Option<String>, ConnectError> {
        let _held = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        Ok(self.read()?.remove(key))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), ConnectError> {
        self.change(|m| {
            m.insert(key.to_string(), value.to_string());
        })
    }

    fn delete(&self, key: &str) -> Result<(), ConnectError> {
        self.change(|m| {
            m.remove(key);
        })
    }
}

fn decode_key(b64: &str) -> Option<[u8; 32]> {
    B64.decode(b64).ok()?.try_into().ok()
}

/// `<root>/secrets.key`, made when there are no secrets yet.
fn key_file(root: &Path, secrets_exist: bool) -> Result<[u8; 32], ConnectError> {
    let path = root.join(KEY_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => decode_key(text.trim()).ok_or_else(|| {
            secrets_error(format!(
                "{} is not a key (32 bytes, base64)",
                path.display()
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if secrets_exist {
                return Err(secrets_error(format!(
                    "{} exists but its key does not: restore {} or set {SECRETS_KEY_VAR}. \
                     To start over without the saved secrets, remove {}",
                    root.join(SECRETS_FILE).display(),
                    path.display(),
                    root.join(SECRETS_FILE).display(),
                )));
            }
            let mut key = [0u8; 32];
            SystemRandom::new()
                .fill(&mut key)
                .map_err(|_| secrets_error("no randomness for a key"))?;
            std::fs::create_dir_all(root)?;
            write_private(&path, format!("{}\n", B64.encode(key)).as_bytes())?;
            Ok(key)
        }
        Err(e) => Err(e.into()),
    }
}

/// Writes `body` to `path` through a temporary file only the owner can read.
fn write_private(path: &Path, body: &[u8]) -> Result<(), ConnectError> {
    use std::io::Write as _;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(body)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real OS keychain, so it runs only when asked:
    /// `cargo test -p ark-connect -- --ignored keychain`.
    #[test]
    #[ignore]
    fn keychain_round_trip() {
        let k = Keychain;
        let key = format!("test:{}", std::process::id());
        assert_eq!(k.get(&key).unwrap(), None);
        k.set(&key, "v1").unwrap();
        assert_eq!(k.get(&key).unwrap().as_deref(), Some("v1"));
        k.set(&key, "v2").unwrap();
        assert_eq!(k.get(&key).unwrap().as_deref(), Some("v2"));
        k.delete(&key).unwrap();
        assert_eq!(k.get(&key).unwrap(), None);
        k.delete(&key).unwrap();
    }

    #[test]
    fn file_secrets_survive_a_restart_and_stay_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileSecrets::in_store(dir.path()).unwrap();
        assert_eq!(s.get("api-key:openai").unwrap(), None);
        s.set("api-key:openai", "sk-secret-value").unwrap();
        s.set("other", "two").unwrap();
        s.delete("other").unwrap();

        // Another process later: the same files.
        let again = FileSecrets::in_store(dir.path()).unwrap();
        assert_eq!(
            again.get("api-key:openai").unwrap().as_deref(),
            Some("sk-secret-value")
        );
        assert_eq!(again.get("other").unwrap(), None);

        let on_disk = std::fs::read_to_string(dir.path().join(SECRETS_FILE)).unwrap();
        assert!(!on_disk.contains("sk-secret-value") && !on_disk.contains("api-key"));
        #[cfg(unix)]
        for f in [SECRETS_FILE, KEY_FILE] {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join(f))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{f}");
        }
    }

    #[test]
    fn file_secrets_refuse_without_their_key() {
        let dir = tempfile::tempdir().unwrap();
        FileSecrets::in_store(dir.path())
            .unwrap()
            .set("k", "sk-secret-value")
            .unwrap();
        std::fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
        let err = FileSecrets::in_store(dir.path()).err().unwrap().to_string();
        assert!(err.contains("exists but its key does not"), "{err}");
        assert!(!err.contains("sk-secret-value"));
        // And no new key was made over the old secrets.
        assert!(!dir.path().join(KEY_FILE).exists());

        let wrong = FileSecrets::with_key(dir.path().join(SECRETS_FILE), [7; 32]);
        let err = wrong.get("k").unwrap_err().to_string();
        assert!(err.contains("cannot be read with this key"), "{err}");
        assert!(!err.contains("sk-secret-value"));
    }

    #[test]
    fn a_given_key_reads_what_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SECRETS_FILE);
        FileSecrets::with_key(&path, [1; 32]).set("k", "v").unwrap();
        assert_eq!(
            FileSecrets::with_key(&path, [1; 32])
                .get("k")
                .unwrap()
                .as_deref(),
            Some("v")
        );
        assert!(!dir.path().join(KEY_FILE).exists());
    }
}
