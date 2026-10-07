//! Access tokens for a runtime that other machines connect to (ADR 0016), and browser sessions
//! entered through one-time links (ADR 0040).
//!
//! A token is created once, shown once, and kept only as its SHA-256 hash in
//! `<store>/access.json` (mode 0600 on Unix). Each has a name ("laptop", "phone") so it can be
//! revoked alone. The file is read on every check, so a revoke takes effect without a restart.
//! A token can do everything the window can: it is the runtime owner's key, not a user account.
//!
//! A link is a one-time code for a browser: whoever runs the runtime (a control plane, a
//! person) makes one with [`Tokens::link`], and the browser trades it once, within a minute or
//! so, for a session ([`Tokens::redeem`]): a token that ends on its own, kept in an HttpOnly
//! cookie and never shown to the page. Sessions carry the link's name, so revoking the name
//! ends every session made from it. Changes to the file hold a lock on `access.json.lock`, so
//! the server and a command on the same store do not lose each other's writes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The saved tokens of one runtime.
#[derive(Clone, Debug)]
pub struct Tokens {
    path: PathBuf,
}

/// A saved token or a session, without its secret.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenInfo {
    pub name: String,
    pub created_ms: u64,
    /// When a session ends (milliseconds since the epoch). None for a saved token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<u64>,
}

impl TokenInfo {
    fn live(&self, now: u64) -> bool {
        self.expires_ms.is_none_or(|t| t > now)
    }
}

/// A session a link was traded for.
#[derive(Clone, Debug)]
pub struct Session {
    pub name: String,
    /// The session's secret: it goes in the browser's cookie and nowhere else.
    pub token: String,
    pub expires_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    #[serde(flatten)]
    info: TokenInfo,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Link {
    name: String,
    sha256: String,
    expires_ms: u64,
    /// How long the session it becomes lasts.
    session_ms: u64,
    /// A short code a person types ([`Tokens::pair_code`]), not a link.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    short: bool,
    /// The session it becomes ends by then at the latest: when the session that made the code
    /// ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    until_ms: Option<u64>,
}

/// Between a name and the name of a device it signed in: `laptop › Phone Safari`. Revoking a
/// name revokes what it signed in.
pub const UNDER: &str = " › ";

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    tokens: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    links: Vec<Link>,
    /// When a typed code was wrong, lately: past [`PAIR_TRIES`] in [`PAIR_WINDOW_MS`], every
    /// open code ends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pair_misses: Vec<u64>,
}

/// Wrong typed codes allowed in [`PAIR_WINDOW_MS`] before every open code ends.
const PAIR_TRIES: usize = 5;
const PAIR_WINDOW_MS: u64 = 10 * 60 * 1000;
/// The letters of a typed code: no 0, 1, I or O to mistake.
const PAIR_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

impl File {
    /// Drop sessions and links whose time is up.
    fn prune(&mut self, now: u64) {
        self.tokens.retain(|t| t.info.live(now));
        self.links.retain(|l| l.expires_ms > now);
        self.pair_misses
            .retain(|t| now.saturating_sub(*t) < PAIR_WINDOW_MS);
    }
}

impl Tokens {
    /// The tokens kept in a store root (`<root>/access.json`).
    pub fn in_store(root: impl AsRef<Path>) -> Tokens {
        Tokens::at(root.as_ref().join("access.json"))
    }

    /// The tokens kept in this file.
    pub fn at(path: impl Into<PathBuf>) -> Tokens {
        Tokens { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create a token named `name` and return it. It is not stored and cannot be shown again.
    pub fn create(&self, name: &str) -> anyhow::Result<String> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("a token needs a name");
        }
        self.update(|file| {
            if file.tokens.iter().any(|t| t.info.name == name) {
                anyhow::bail!("a token named `{name}` exists; revoke it first");
            }
            let token = secret("ark_");
            file.tokens.push(Entry {
                info: TokenInfo {
                    name: name.to_string(),
                    created_ms: now_ms(),
                    expires_ms: None,
                },
                sha256: hash(&token),
            });
            Ok(token)
        })
    }

    /// A one-time link code named `name`, good for `valid_for`, that a browser trades for a
    /// session lasting `session` ([`redeem`](Self::redeem)). Only its hash is kept. A saved
    /// token's name cannot be used: revoking a name would end both.
    pub fn link(
        &self,
        name: &str,
        valid_for: Duration,
        session: Duration,
    ) -> anyhow::Result<String> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("a link needs a name");
        }
        self.update(|file| {
            if file
                .tokens
                .iter()
                .any(|t| t.info.name == name && t.info.expires_ms.is_none())
            {
                anyhow::bail!("a saved token is named `{name}`; links need another name");
            }
            let code = secret("arkl_");
            file.links.push(Link {
                name: name.to_string(),
                sha256: hash(&code),
                expires_ms: now_ms() + valid_for.as_millis() as u64,
                session_ms: session.as_millis() as u64,
                short: false,
                until_ms: None,
            });
            Ok(code)
        })
    }

    /// A code a person types on a device that cannot open a link (a web app on a phone's Home
    /// Screen has cookies of its own): `K7Q4-ZP2M`, good for `valid_for`, traded once
    /// ([`Tokens::redeem_as`]) for a session lasting `session` and named under `name` (`name ›
    /// <the device>`), so revoking `name` signs the device out too. `until_ms`: the session
    /// ends by then at the latest (the session that made the code ends then). A few wrong codes
    /// end every open one.
    pub fn pair_code(
        &self,
        name: &str,
        valid_for: Duration,
        session: Duration,
        until_ms: Option<u64>,
    ) -> anyhow::Result<String> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("a device needs a name");
        }
        self.update(|file| {
            let raw: String = uuid::Uuid::new_v4()
                .as_bytes()
                .iter()
                .take(8)
                .map(|b| PAIR_ALPHABET[(*b % 32) as usize] as char)
                .collect();
            file.links.push(Link {
                name: name.to_string(),
                sha256: hash(&raw),
                expires_ms: now_ms() + valid_for.as_millis() as u64,
                session_ms: session.as_millis() as u64,
                short: true,
                until_ms,
            });
            Ok(format!("{}-{}", &raw[..4], &raw[4..]))
        })
    }

    /// Trade a link code for a session, once. None when the code is unknown, used or late.
    pub fn redeem(&self, code: &str) -> anyhow::Result<Option<Session>> {
        self.redeem_as(code, "")
    }

    /// [`redeem`](Self::redeem), from a device that says what it is (`Phone Safari`): a typed
    /// code's session is named `<who made the code> › <device>`.
    pub fn redeem_as(&self, code: &str, device: &str) -> anyhow::Result<Option<Session>> {
        let code = code.trim();
        if code.is_empty() {
            return Ok(None);
        }
        // A typed code: any case, with or without its dash and spaces.
        let typed: String = code
            .chars()
            .filter(|c| !matches!(c, '-' | ' '))
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let short = typed.len() == 8 && typed.bytes().all(|b| PAIR_ALPHABET.contains(&b));
        let h = hash(if short { &typed } else { code });
        self.update(|file| {
            let found = file
                .links
                .iter()
                .position(|l| same(&l.sha256, &h) && l.short == short);
            let Some(i) = found else {
                if short {
                    file.pair_misses.push(now_ms());
                    let extra = file.pair_misses.len().saturating_sub(PAIR_TRIES);
                    file.pair_misses.drain(..extra);
                    if file.pair_misses.len() >= PAIR_TRIES {
                        file.links.retain(|l| !l.short);
                    }
                }
                return Ok(None);
            };
            let link = file.links.remove(i);
            let now = now_ms();
            let token = secret("arks_");
            let expires_ms = (now + link.session_ms).min(link.until_ms.unwrap_or(u64::MAX));
            // A typed code's device is named under who made the code, and apart from the
            // devices signed in before it.
            let name = if link.short {
                // A device names itself, but not as if it were under another one.
                let device: String = device
                    .replace(UNDER.trim(), "-")
                    .trim()
                    .chars()
                    .take(40)
                    .collect();
                let base = format!(
                    "{}{UNDER}{}",
                    link.name,
                    if device.is_empty() {
                        "device"
                    } else {
                        device.as_str()
                    }
                );
                let taken = |n: &str| file.tokens.iter().any(|t| t.info.name == n);
                let mut name = base.clone();
                let mut k = 2;
                while taken(&name) {
                    name = format!("{base} {k}");
                    k += 1;
                }
                name
            } else {
                link.name.clone()
            };
            file.tokens.push(Entry {
                info: TokenInfo {
                    name: name.clone(),
                    created_ms: now,
                    expires_ms: Some(expires_ms),
                },
                sha256: hash(&token),
            });
            Ok(Some(Session {
                name,
                token,
                expires_ms,
            }))
        })
    }

    /// End the one session or token `token` is (a browser signing out). False when unknown.
    pub fn end(&self, token: &str) -> anyhow::Result<bool> {
        if token.is_empty() {
            return Ok(false);
        }
        let h = hash(token);
        self.update(|file| {
            let before = file.tokens.len();
            file.tokens.retain(|t| !same(&t.sha256, &h));
            Ok(file.tokens.len() != before)
        })
    }

    /// Every saved token and live session, oldest first.
    pub fn list(&self) -> anyhow::Result<Vec<TokenInfo>> {
        let now = now_ms();
        Ok(self
            .read()?
            .tokens
            .into_iter()
            .map(|t| t.info)
            .filter(|t| t.live(now))
            .collect())
    }

    /// Remove the token named `name`, with every session and unused link of that name. False
    /// when there is none.
    pub fn revoke(&self, name: &str) -> anyhow::Result<bool> {
        // What it signed in goes with it (`name › device`).
        let under = format!("{name}{UNDER}");
        let gone = |n: &str| n == name || n.starts_with(&under);
        self.update(|file| {
            let before = file.tokens.len() + file.links.len();
            file.tokens.retain(|t| !gone(&t.info.name));
            file.links.retain(|l| !gone(&l.name));
            Ok(file.tokens.len() + file.links.len() != before)
        })
    }

    /// The saved token or live session `token` is, without its secret.
    pub fn info(&self, token: &str) -> Option<TokenInfo> {
        if token.is_empty() {
            return None;
        }
        let (h, now) = (hash(token), now_ms());
        self.read()
            .ok()?
            .tokens
            .into_iter()
            .find(|t| same(&t.sha256, &h) && t.info.live(now))
            .map(|t| t.info)
    }

    /// The name of the saved token or live session `token` is, if any.
    pub fn check(&self, token: &str) -> Option<String> {
        if token.is_empty() {
            return None;
        }
        let (h, now) = (hash(token), now_ms());
        self.read()
            .ok()?
            .tokens
            .into_iter()
            .find(|t| same(&t.sha256, &h) && t.info.live(now))
            .map(|t| t.info.name)
    }

    /// Read, change and write the file under its lock; expired sessions and links go first.
    fn update<T>(&self, f: impl FnOnce(&mut File) -> anyhow::Result<T>) -> anyhow::Result<T> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.with_extension("json.lock"))?;
        lock.lock()?;
        let mut file = self.read()?;
        file.prune(now_ms());
        let out = f(&mut file)?;
        self.write(&file)?;
        Ok(out)
    }

    fn read(&self) -> anyhow::Result<File> {
        match std::fs::read(&self.path) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
            Err(e) => Err(anyhow::anyhow!("reading {}: {e}", self.path.display())),
        }
    }

    fn write(&self, file: &File) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(file)?)?;
        ark_os::fs::owner_only(&tmp)?;
        ark_os::fs::replace(&tmp, &self.path)?;
        Ok(())
    }
}

/// A random secret with a prefix that says what it is: 64 hex characters, 256 bits.
fn secret(prefix: &str) -> String {
    format!(
        "{prefix}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Compare without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_code_pairs_once_and_wrong_ones_end_it() {
        let dir = tempfile::tempdir().unwrap();
        let t = Tokens::in_store(dir.path());
        let day = Duration::from_secs(24 * 3600);
        let laptop = t.create("laptop").unwrap();
        let code = t
            .pair_code("laptop", Duration::from_secs(600), day, None)
            .unwrap();
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        // Typed in lower case without the dash: the same code, once, named under its maker.
        let s = t
            .redeem_as(&code.replace('-', "").to_lowercase(), "Phone Safari")
            .unwrap()
            .unwrap();
        assert_eq!(s.name, "laptop › Phone Safari");
        assert_eq!(t.check(&s.token).as_deref(), Some("laptop › Phone Safari"));
        assert!(t.redeem(&code).unwrap().is_none(), "used");
        // Wrong codes: past five in ten minutes, every open code ends.
        let code = t
            .pair_code("laptop", Duration::from_secs(600), day, None)
            .unwrap();
        for _ in 0..5 {
            assert!(t.redeem("2222-2222").unwrap().is_none());
        }
        assert!(
            t.redeem(&code).unwrap().is_none(),
            "ended by the wrong tries"
        );
        // A link is not a typed code and is not ended by them.
        let link = t.link("web", Duration::from_secs(60), day).unwrap();
        assert!(t.redeem(&link).unwrap().is_some());
        // Revoking who made a code signs out the devices it signed in; a cap holds.
        assert!(t.revoke("laptop").unwrap());
        assert!(t.check(&s.token).is_none(), "signed out with its maker");
        assert!(t.check(&laptop).is_none());
        let soon = now_ms() + 60_000;
        let code = t
            .pair_code("web", Duration::from_secs(600), day, Some(soon))
            .unwrap();
        let s = t.redeem_as(&code, "").unwrap().unwrap();
        assert_eq!(s.expires_ms, soon, "not past the session that made it");
        assert_eq!(s.name, "web › device");
    }

    #[test]
    fn create_check_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::in_store(dir.path());
        assert_eq!(tokens.check("ark_nothing"), None);
        let t = tokens.create("laptop").unwrap();
        assert!(t.starts_with("ark_") && t.len() == 68);
        assert_eq!(tokens.check(&t).as_deref(), Some("laptop"));
        assert!(tokens.create("laptop").is_err(), "names are unique");
        let raw = std::fs::read_to_string(tokens.path()).unwrap();
        assert!(!raw.contains(&t), "only the hash is stored");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tokens.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(tokens.revoke("laptop").unwrap());
        assert!(!tokens.revoke("laptop").unwrap());
        assert_eq!(tokens.check(&t), None);
        assert!(tokens.list().unwrap().is_empty());
    }

    #[test]
    fn a_link_becomes_one_session_that_ends_on_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::in_store(dir.path());
        let hour = Duration::from_secs(3600);
        let code = tokens
            .link("Chrome on Mac", Duration::from_secs(60), hour)
            .unwrap();
        assert!(code.starts_with("arkl_"));
        assert_eq!(tokens.check(&code), None, "a link is not a token");
        let raw = std::fs::read_to_string(tokens.path()).unwrap();
        assert!(!raw.contains(&code), "only the hash is stored");

        let s = tokens.redeem(&code).unwrap().expect("a fresh code");
        assert_eq!(s.name, "Chrome on Mac");
        assert!(s.token.starts_with("arks_"));
        assert_eq!(tokens.check(&s.token).as_deref(), Some("Chrome on Mac"));
        assert!(tokens.redeem(&code).unwrap().is_none(), "once");
        let listed = tokens.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].expires_ms.is_some());

        // A late code and an ended session.
        let late = tokens.link("Chrome on Mac", Duration::ZERO, hour).unwrap();
        assert!(tokens.redeem(&late).unwrap().is_none());
        let short = tokens
            .link("Safari on a phone", Duration::from_secs(60), Duration::ZERO)
            .unwrap();
        let gone = tokens.redeem(&short).unwrap().unwrap();
        assert_eq!(tokens.check(&gone.token), None, "a session ends on its own");

        // Ending one session leaves the others; revoking the name ends them all, links too.
        let a = tokens
            .redeem(
                &tokens
                    .link("Chrome on Mac", Duration::from_secs(60), hour)
                    .unwrap(),
            )
            .unwrap()
            .unwrap();
        assert!(tokens.end(&s.token).unwrap());
        assert_eq!(tokens.check(&s.token), None);
        assert!(tokens.check(&a.token).is_some());
        let unused = tokens
            .link("Chrome on Mac", Duration::from_secs(60), hour)
            .unwrap();
        assert!(tokens.revoke("Chrome on Mac").unwrap());
        assert_eq!(tokens.check(&a.token), None);
        assert!(tokens.redeem(&unused).unwrap().is_none());

        // Names stay apart from saved tokens'.
        tokens.create("laptop").unwrap();
        assert!(tokens
            .link("laptop", Duration::from_secs(60), hour)
            .is_err());
    }
}
